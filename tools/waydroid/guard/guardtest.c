// Probe run inside a throwaway LXC container by tools/waydroid/container-guard.sh test:
// checks that the container guard blocks what our AOSP 17 image did to the host
// (EXT4_IOC_SHUTDOWN on a bind mount of the host's /, 2026-11-18) and access to the
// host's disks. Writes one line per check to /result and exits.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/sysmacros.h>
#include <unistd.h>

static FILE* out;

// The raw syscall, so the request reaches the kernel exactly as given (bionic passes an
// int request sign-extended to 64 bits; the guard must catch both forms).
static void check_ioctl(const char* name, int fd, unsigned long req) {
    long r = syscall(SYS_ioctl, fd, req, 0);
    int e = errno;
    fprintf(out, "%s %s ioctl %#lx -> %s\n", r == -1 && e == EPERM ? "PASS" : "FAIL", name, req,
            r == -1 ? strerror(e) : "ok");
}

static void check_dev(const char* name, unsigned maj, unsigned min, int want_blocked) {
    char path[64];
    snprintf(path, sizeof(path), "/dev/guardtest-%u-%u", maj, min);
    unlink(path);
    if (mknod(path, S_IFBLK | 0600, makedev(maj, min)) != 0) {
        fprintf(out, "%s %s mknod -> %s\n", want_blocked ? "PASS" : "FAIL", name, strerror(errno));
        return;
    }
    int fd = open(path, O_RDONLY);
    int e = errno;
    // EPERM comes from the device cgroup; anything else (ENXIO, ...) means it got through.
    int blocked = fd < 0 && e == EPERM;
    fprintf(out, "%s %s open %u:%u -> %s\n", blocked == want_blocked ? "PASS" : "FAIL", name, maj,
            min, fd < 0 ? strerror(e) : "ok");
    if (fd >= 0) close(fd);
    unlink(path);
}

int main(void) {
    out = fopen("/result", "w");
    if (!out) return 1;
    // /dev is a tmpfs: without the guard these ioctls fail with ENOTTY there instead of
    // reaching a real filesystem, so a broken guard cannot hurt the host during the test.
    int fd = open("/dev", O_RDONLY | O_DIRECTORY);
    check_ioctl("EXT4_IOC_SHUTDOWN", fd, 0x8004587dUL);
    check_ioctl("EXT4_IOC_SHUTDOWN(sign-extended)", fd, 0xffffffff8004587dUL);
    check_ioctl("F2FS_IOC_SHUTDOWN", fd, 0x8004f519UL);
    check_ioctl("FIFREEZE", fd, 0xc0045877UL);
    check_ioctl("FIFREEZE(sign-extended)", fd, 0xffffffffc0045877UL);
    check_ioctl("FITHAW", fd, 0xc0045878UL);
    // A harmless ioctl must still work (FIONREAD-like probe on a pipe).
    int p[2];
    int n = 0;
    pipe(p);
    long r = syscall(SYS_ioctl, p[0], (unsigned long)FIONREAD, &n);
    fprintf(out, "%s FIONREAD on a pipe -> %s\n", r == 0 ? "PASS" : "FAIL",
            r == 0 ? "ok" : strerror(errno));
    check_dev("nvme", 259, 0, 1);
    check_dev("sd", 8, 0, 1);
    check_dev("dm", 254, 0, 1);
    check_dev("loop", 7, 0, 0);
    fclose(out);
    return 0;
}
