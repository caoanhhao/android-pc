// JPEG codec for the PC camera HAL; see pcjpeg.c. Mirrored in src/jpeg.rs.

#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The most luma rows in one MCU row (4:2:0 and 4:2:2 use 16; libjpeg allows up to 4x8).
#define PCJPEG_MAX_MCU_ROWS 32

// An image's planes: Y, then Cb and Cr when `components` is 3.
struct pcjpeg_info {
    int width;
    int height;
    int components;
    // Visible samples of each plane.
    int plane_width[3];
    int plane_height[3];
    // The buffer each plane needs: bytes per row and rows (whole MCUs).
    int plane_stride[3];
    int plane_rows[3];
};

// Reads `jpeg`'s header into `info`; with non-null `planes` (buffers sized as `info`
// says) also decodes the image into them. Returns 0, or -1 with a reason in `message`.
int pcjpeg_decode(const uint8_t* jpeg, size_t jpeg_len, struct pcjpeg_info* info,
                  uint8_t* const* planes, char* message, size_t message_len);

// Encodes a tightly packed I420 image as baseline 4:2:0 JPEG. On success `*jpeg` is a
// malloc'ed buffer of `*jpeg_len` bytes for the caller to free. Returns 0, or -1 with a
// reason in `message`.
int pcjpeg_encode_i420(const uint8_t* y, const uint8_t* cb, const uint8_t* cr, int width,
                       int height, int quality, uint8_t** jpeg, size_t* jpeg_len, char* message,
                       size_t message_len);

#ifdef __cplusplus
}
#endif
