#include <sys/types.h>
#include <errno.h>

/*
 * FreeBSD 12 libc does not export `copy_file_range` (introduced in FreeBSD 13.0).
 * When cross-compiling for FreeBSD via cross-rs (which uses a FreeBSD 12 sysroot),
 * libghostty-vt static library (compiled by Zig) contains a reference to
 * `copy_file_range` from Zig's standard library.
 *
 * Providing this fallback implementation allows linking against FreeBSD 12 libc
 * to succeed. If called at runtime on FreeBSD 12, it returns ENOSYS (-1), causing
 * Zig's stdlib to gracefully fall back to standard read/write loops.
 */
ssize_t copy_file_range(int fd_in, off_t *off_in, int fd_out, off_t *off_out, size_t len, unsigned int flags) {
    (void)fd_in;
    (void)off_in;
    (void)fd_out;
    (void)off_out;
    (void)len;
    (void)flags;
    errno = ENOSYS;
    return -1;
}
