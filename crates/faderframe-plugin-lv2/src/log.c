// The LV2 log feature's functions take printf-style arguments, which
// stable Rust cannot receive: they are formatted here and the message
// handed to the host (Rust) as a string.
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>

void ff_lv2_log_message(void *handle, uint32_t type, const char *message);

int ff_lv2_log_vprintf(void *handle, uint32_t type, const char *fmt, va_list ap) {
    char buf[1024];
    int n = vsnprintf(buf, sizeof buf, fmt, ap);
    ff_lv2_log_message(handle, type, buf);
    return n;
}

int ff_lv2_log_printf(void *handle, uint32_t type, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = ff_lv2_log_vprintf(handle, type, fmt, ap);
    va_end(ap);
    return r;
}
