// JPEG decoding and encoding on planar YCbCr through libjpeg's raw data interface.
//
// AOSP's libjpeg (libjpeg-turbo) does not include the TurboJPEG API, and libjpeg reports
// errors with longjmp, which Rust code must not be unwound by. This file keeps every
// setjmp/longjmp in C and gives src/jpeg.rs plain status codes.

#include "pcjpeg.h"

#include <setjmp.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <jpeglib.h>

struct error_manager {
    struct jpeg_error_mgr pub;
    jmp_buf jump;
    char* message;
    size_t message_len;
};

static void error_exit(j_common_ptr cinfo) {
    struct error_manager* err = (struct error_manager*)cinfo->err;
    if (err->message_len > 0) {
        char buffer[JMSG_LENGTH_MAX];
        err->pub.format_message(cinfo, buffer);
        snprintf(err->message, err->message_len, "%s", buffer);
    }
    longjmp(err->jump, 1);
}

// Webcams send slightly damaged frames now and then; libjpeg's warnings about them only
// fill the log.
static void emit_message(j_common_ptr cinfo, int level) {
    (void)cinfo;
    (void)level;
}

static void init_errors(struct error_manager* err, char* message, size_t message_len) {
    jpeg_std_error(&err->pub);
    err->pub.error_exit = error_exit;
    err->pub.emit_message = emit_message;
    err->message = message;
    err->message_len = message_len;
    if (message_len > 0) message[0] = '\0';
}

static int round_up(int value, int multiple) {
    return (value + multiple - 1) / multiple * multiple;
}

static void fill_info(const struct jpeg_decompress_struct* cinfo, struct pcjpeg_info* info) {
    int mcu_width = cinfo->max_h_samp_factor * DCTSIZE;
    int mcu_height = cinfo->max_v_samp_factor * DCTSIZE;
    info->width = (int)cinfo->image_width;
    info->height = (int)cinfo->image_height;
    info->components = cinfo->num_components;
    for (int c = 0; c < cinfo->num_components && c < 3; c++) {
        const jpeg_component_info* comp = &cinfo->comp_info[c];
        // Visible samples, rounded up as libjpeg's downsampling does.
        info->plane_width[c] = (int)((cinfo->image_width * comp->h_samp_factor +
                                      cinfo->max_h_samp_factor - 1) /
                                     cinfo->max_h_samp_factor);
        info->plane_height[c] = (int)((cinfo->image_height * comp->v_samp_factor +
                                       cinfo->max_v_samp_factor - 1) /
                                      cinfo->max_v_samp_factor);
        // Raw output writes whole MCUs, so the buffers cover them.
        info->plane_stride[c] =
                round_up((int)cinfo->image_width, mcu_width) / mcu_width * comp->h_samp_factor *
                DCTSIZE;
        info->plane_rows[c] = round_up((int)cinfo->image_height, mcu_height) / mcu_height *
                              comp->v_samp_factor * DCTSIZE;
    }
}

static int supported(const struct jpeg_decompress_struct* cinfo) {
    if (cinfo->num_components == 1) return cinfo->jpeg_color_space == JCS_GRAYSCALE;
    return cinfo->num_components == 3 && cinfo->jpeg_color_space == JCS_YCbCr &&
           cinfo->max_v_samp_factor * DCTSIZE <= PCJPEG_MAX_MCU_ROWS;
}

int pcjpeg_decode(const uint8_t* jpeg, size_t jpeg_len, struct pcjpeg_info* info,
                  uint8_t* const* planes, char* message, size_t message_len) {
    struct jpeg_decompress_struct cinfo;
    struct error_manager err;
    init_errors(&err, message, message_len);
    cinfo.err = &err.pub;
    if (setjmp(err.jump)) {
        jpeg_destroy_decompress(&cinfo);
        return -1;
    }
    jpeg_create_decompress(&cinfo);
    jpeg_mem_src(&cinfo, jpeg, (unsigned long)jpeg_len);
    jpeg_read_header(&cinfo, TRUE);
    if (!supported(&cinfo)) {
        snprintf(message, message_len, "unsupported colour space or sampling");
        jpeg_destroy_decompress(&cinfo);
        return -1;
    }
    fill_info(&cinfo, info);
    if (planes == NULL) {
        jpeg_destroy_decompress(&cinfo);
        return 0;
    }

    cinfo.raw_data_out = TRUE;
    cinfo.out_color_space = cinfo.jpeg_color_space;
    cinfo.dct_method = JDCT_IFAST;
    jpeg_start_decompress(&cinfo);
    int mcu_height = cinfo.max_v_samp_factor * DCTSIZE;
    JSAMPROW rows[3][PCJPEG_MAX_MCU_ROWS];
    JSAMPARRAY arrays[3] = {rows[0], rows[1], rows[2]};
    while (cinfo.output_scanline < cinfo.output_height) {
        int mcu_row = (int)cinfo.output_scanline / mcu_height;
        for (int c = 0; c < cinfo.num_components; c++) {
            int comp_rows = cinfo.comp_info[c].v_samp_factor * DCTSIZE;
            for (int r = 0; r < comp_rows; r++) {
                rows[c][r] = planes[c] + (size_t)(mcu_row * comp_rows + r) * info->plane_stride[c];
            }
        }
        if (jpeg_read_raw_data(&cinfo, arrays, (JDIMENSION)mcu_height) == 0) {
            snprintf(message, message_len, "truncated data");
            jpeg_destroy_decompress(&cinfo);
            return -1;
        }
    }
    jpeg_finish_decompress(&cinfo);
    jpeg_destroy_decompress(&cinfo);
    return 0;
}

// Copies a width x height plane into a stride x rows buffer, repeating the last column and
// row into the padding.
static uint8_t* padded_copy(const uint8_t* src, int width, int height, int stride, int rows) {
    uint8_t* dst = malloc((size_t)stride * rows);
    if (dst == NULL) return NULL;
    for (int y = 0; y < rows; y++) {
        const uint8_t* line = src + (size_t)(y < height ? y : height - 1) * width;
        uint8_t* out = dst + (size_t)y * stride;
        memcpy(out, line, width);
        memset(out + width, line[width - 1], stride - width);
    }
    return dst;
}

int pcjpeg_encode_i420(const uint8_t* y, const uint8_t* cb, const uint8_t* cr, int width,
                       int height, int quality, uint8_t** jpeg, size_t* jpeg_len, char* message,
                       size_t message_len) {
    int chroma_width = (width + 1) / 2;
    int chroma_height = (height + 1) / 2;
    int stride = round_up(width, 2 * DCTSIZE);
    int rows = round_up(height, 2 * DCTSIZE);
    uint8_t* planes[3] = {
            padded_copy(y, width, height, stride, rows),
            padded_copy(cb, chroma_width, chroma_height, stride / 2, rows / 2),
            padded_copy(cr, chroma_width, chroma_height, stride / 2, rows / 2),
    };
    unsigned char* out = NULL;
    unsigned long out_len = 0;
    // volatile: read after a longjmp.
    volatile int ret = -1;

    struct jpeg_compress_struct cinfo;
    struct error_manager err;
    init_errors(&err, message, message_len);
    cinfo.err = &err.pub;
    jpeg_create_compress(&cinfo);
    if (planes[0] == NULL || planes[1] == NULL || planes[2] == NULL) {
        snprintf(message, message_len, "out of memory");
        goto done;
    }
    if (setjmp(err.jump)) goto done;

    jpeg_mem_dest(&cinfo, &out, &out_len);
    cinfo.image_width = (JDIMENSION)width;
    cinfo.image_height = (JDIMENSION)height;
    cinfo.input_components = 3;
    cinfo.in_color_space = JCS_YCbCr;
    jpeg_set_defaults(&cinfo);
    jpeg_set_quality(&cinfo, quality, TRUE);
    cinfo.raw_data_in = TRUE;
    cinfo.dct_method = JDCT_ISLOW;
    cinfo.comp_info[0].h_samp_factor = 2;
    cinfo.comp_info[0].v_samp_factor = 2;
    for (int c = 1; c < 3; c++) {
        cinfo.comp_info[c].h_samp_factor = 1;
        cinfo.comp_info[c].v_samp_factor = 1;
    }
    jpeg_start_compress(&cinfo, TRUE);
    JSAMPROW luma[2 * DCTSIZE], blue[DCTSIZE], red[DCTSIZE];
    JSAMPARRAY arrays[3] = {luma, blue, red};
    while (cinfo.next_scanline < cinfo.image_height) {
        int top = (int)cinfo.next_scanline;
        for (int r = 0; r < 2 * DCTSIZE; r++) luma[r] = planes[0] + (size_t)(top + r) * stride;
        for (int r = 0; r < DCTSIZE; r++) {
            blue[r] = planes[1] + (size_t)(top / 2 + r) * (stride / 2);
            red[r] = planes[2] + (size_t)(top / 2 + r) * (stride / 2);
        }
        jpeg_write_raw_data(&cinfo, arrays, 2 * DCTSIZE);
    }
    jpeg_finish_compress(&cinfo);
    *jpeg = out;
    *jpeg_len = out_len;
    out = NULL;
    ret = 0;

done:
    jpeg_destroy_compress(&cinfo);
    free(out);
    for (int c = 0; c < 3; c++) free(planes[c]);
    return ret;
}
