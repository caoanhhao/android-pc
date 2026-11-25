// Minimal V4L2 stateful decoder test (kernel's dev-decoder.rst flow) for the SM8250 video codec.
//   v4l2dec DEVICE H264|HEVC|VP90 IN.hdr [OUT.y]
// IN.hdr holds one access unit per buffer (v4l2-hdr.py). Prints the coded size the driver
// reports, the frame count and the time; with OUT.y it writes each frame's luma (width x
// height, no padding) for comparison with a software decode. v4l2-ctl cannot do this here: it
// treats qcom-iris as no codec because the capture queue lists compressed formats (Q08C).
// Build: aarch64-linux-android30-clang -O2 -static v4l2dec.c -o v4l2dec
#include <errno.h>
#include <fcntl.h>
#include <linux/videodev2.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define NOUT 8
#define MAXCAP 32

static int fd;
static FILE *in, *out;
static void *outmap[NOUT];
static uint32_t outlen;
static void *capmap[MAXCAP];
static uint32_t caplen[MAXCAP];
static unsigned ncap;
static struct v4l2_format capfmt;

static void die(const char *what) {
    fprintf(stderr, "%s: %s\n", what, strerror(errno));
    exit(1);
}

static void xioctl(unsigned long req, void *arg, const char *name) {
    if (ioctl(fd, req, arg) < 0) die(name);
}

static uint32_t be32(const uint8_t *p) { return p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3]; }

// Fills output buffer i with the next access unit; returns 0 at the end of the file.
static int fill(unsigned i) {
    uint8_t hdr[8];
    if (fread(hdr, 1, 8, in) != 8) return 0;
    uint32_t len = be32(hdr + 4);
    if (len > outlen || fread(outmap[i], 1, len, in) != len) {
        fprintf(stderr, "bad unit (%u bytes)\n", len);
        exit(1);
    }
    struct v4l2_plane pl = {.bytesused = len};
    struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, .memory = V4L2_MEMORY_MMAP,
                            .index = i, .length = 1, .m.planes = &pl};
    static long ts;
    b.timestamp.tv_usec = ts++;
    xioctl(VIDIOC_QBUF, &b, "QBUF out");
    return 1;
}

static void qcap(unsigned i) {
    struct v4l2_plane pl = {0};
    struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, .memory = V4L2_MEMORY_MMAP,
                            .index = i, .length = 1, .m.planes = &pl};
    xioctl(VIDIOC_QBUF, &b, "QBUF cap");
}

static void setup_capture(void) {
    capfmt.type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    xioctl(VIDIOC_G_FMT, &capfmt, "G_FMT cap");
    struct v4l2_control min = {.id = V4L2_CID_MIN_BUFFERS_FOR_CAPTURE};
    ioctl(fd, VIDIOC_G_CTRL, &min);
    struct v4l2_requestbuffers rb = {.count = min.value + 4,
                                     .type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                                     .memory = V4L2_MEMORY_MMAP};
    if (rb.count > MAXCAP) rb.count = MAXCAP;
    xioctl(VIDIOC_REQBUFS, &rb, "REQBUFS cap");
    ncap = rb.count;
    struct v4l2_pix_format_mplane *p = &capfmt.fmt.pix_mp;
    printf("capture %.4s %ux%u stride %u size %u, min buffers %d, %u buffers\n",
           (char *)&p->pixelformat, p->width, p->height, p->plane_fmt[0].bytesperline,
           p->plane_fmt[0].sizeimage, min.value, ncap);
    for (unsigned i = 0; i < ncap; i++) {
        struct v4l2_plane pl = {0};
        struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                                .memory = V4L2_MEMORY_MMAP, .index = i, .length = 1,
                                .m.planes = &pl};
        xioctl(VIDIOC_QUERYBUF, &b, "QUERYBUF cap");
        caplen[i] = pl.length;
        capmap[i] = mmap(NULL, pl.length, PROT_READ, MAP_SHARED, fd, pl.m.mem_offset);
        if (capmap[i] == MAP_FAILED) die("mmap cap");
        qcap(i);
    }
    int type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    xioctl(VIDIOC_STREAMON, &type, "STREAMON cap");
}

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: v4l2dec DEVICE H264|HEVC|VP90 IN.hdr [OUT.y]\n");
        return 2;
    }
    fd = open(argv[1], O_RDWR | O_NONBLOCK);
    if (fd < 0) die(argv[1]);
    in = fopen(argv[3], "rb");
    if (!in) die(argv[3]);
    if (argc > 4 && !(out = fopen(argv[4], "wb"))) die(argv[4]);

    struct v4l2_format f = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE};
    f.fmt.pix_mp.pixelformat = argv[2][0] | argv[2][1] << 8 | argv[2][2] << 16 | argv[2][3] << 24;
    f.fmt.pix_mp.width = 1280;
    f.fmt.pix_mp.height = 720;
    f.fmt.pix_mp.num_planes = 1;
    f.fmt.pix_mp.plane_fmt[0].sizeimage = 2 << 20;
    xioctl(VIDIOC_S_FMT, &f, "S_FMT out");
    struct v4l2_event_subscription sub = {.type = V4L2_EVENT_SOURCE_CHANGE};
    xioctl(VIDIOC_SUBSCRIBE_EVENT, &sub, "SUBSCRIBE source change");
    sub.type = V4L2_EVENT_EOS;
    ioctl(fd, VIDIOC_SUBSCRIBE_EVENT, &sub);

    struct v4l2_requestbuffers rb = {.count = NOUT, .type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
                                     .memory = V4L2_MEMORY_MMAP};
    xioctl(VIDIOC_REQBUFS, &rb, "REQBUFS out");
    unsigned nout = rb.count < NOUT ? rb.count : NOUT;
    for (unsigned i = 0; i < nout; i++) {
        struct v4l2_plane pl = {0};
        struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
                                .memory = V4L2_MEMORY_MMAP, .index = i, .length = 1,
                                .m.planes = &pl};
        xioctl(VIDIOC_QUERYBUF, &b, "QUERYBUF out");
        outlen = pl.length;
        outmap[i] = mmap(NULL, pl.length, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                         pl.m.mem_offset);
        if (outmap[i] == MAP_FAILED) die("mmap out");
    }
    int eof = 0, queued = 0;
    for (unsigned i = 0; i < nout && !eof; i++) {
        if (fill(i)) queued++;
        else eof = 1;
    }
    int type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    xioctl(VIDIOC_STREAMON, &type, "STREAMON out");

    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    int stopped = 0, done = 0, frames = 0, changes = 0;
    while (!done) {
        if (eof && !stopped) {
            struct v4l2_decoder_cmd cmd = {.cmd = V4L2_DEC_CMD_STOP};
            xioctl(VIDIOC_DECODER_CMD, &cmd, "DECODER_CMD stop");
            stopped = 1;
        }
        struct pollfd pfd = {.fd = fd, .events = POLLIN | POLLOUT | POLLPRI};
        int r = poll(&pfd, 1, 5000);
        if (r == 0) {
            fprintf(stderr, "timeout after %d frames\n", frames);
            return 1;
        }
        if (pfd.revents & POLLPRI) {
            struct v4l2_event ev;
            while (ioctl(fd, VIDIOC_DQEVENT, &ev) == 0) {
                if (ev.type == V4L2_EVENT_SOURCE_CHANGE && !changes++) setup_capture();
                else if (ev.type == V4L2_EVENT_EOS) printf("EOS event\n");
            }
        }
        if (pfd.revents & POLLOUT) {
            struct v4l2_plane pl = {0};
            struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
                                    .memory = V4L2_MEMORY_MMAP, .length = 1, .m.planes = &pl};
            while (ioctl(fd, VIDIOC_DQBUF, &b) == 0) {
                if (!eof) {
                    if (fill(b.index)) queued++;
                    else eof = 1;
                }
            }
        }
        if ((pfd.revents & POLLIN) && ncap) {
            struct v4l2_plane pl = {0};
            struct v4l2_buffer b = {.type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE,
                                    .memory = V4L2_MEMORY_MMAP, .length = 1, .m.planes = &pl};
            while (ioctl(fd, VIDIOC_DQBUF, &b) == 0) {
                if (pl.bytesused) {
                    frames++;
                    if (out) {
                        struct v4l2_pix_format_mplane *p = &capfmt.fmt.pix_mp;
                        for (unsigned y = 0; y < p->height; y++)
                            fwrite((uint8_t *)capmap[b.index] + y * p->plane_fmt[0].bytesperline,
                                   1, p->width, out);
                    }
                }
                if (b.flags & V4L2_BUF_FLAG_LAST) {
                    done = 1;
                    break;
                }
                qcap(b.index);
            }
        }
        if (pfd.revents & POLLERR && !ncap) {
            fprintf(stderr, "POLLERR before the source change\n");
            usleep(1000);
        }
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    double ms = (t1.tv_sec - t0.tv_sec) * 1e3 + (t1.tv_nsec - t0.tv_nsec) / 1e6;
    printf("%s: %d units in, %d frames out in %.0f ms (%.0f fps)\n", argv[3], queued, frames, ms,
           frames * 1000.0 / ms);
    if (out) fclose(out);
    return frames > 0 ? 0 : 1;
}
