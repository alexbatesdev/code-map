#ifndef MEGAMAP_NATIVE_H
#define MEGAMAP_NATIVE_H
#include <stddef.h>
#include <stdint.h>

typedef void MmContext;
typedef struct {
    float rect[4], uv[4], color[4], clip[4], edge[4], params[4];
} MmQuad;

typedef struct {
    uint32_t kind, code, modifiers, button, clicks;
    double x, y, dx, dy, width, height, scale;
    const uint8_t *text;
    size_t text_len;
    uint64_t selection_start, selection_len, replace_start, replace_len, id;
} MmEvent;

typedef struct {
    uint64_t instances, draw_calls, blits, atlas_glyphs, atlas_used_bytes;
    uint64_t atlas_upload_bytes, dropped_instances, dropped_glyphs, skipped;
    double cpu_ms, gpu_ms;
} MmFrameStats;

typedef struct {
    uint64_t id;
    uint32_t role, enabled, focused;
    double rect[4];
    const char *label, *value;
} MmAccessibility;

MmContext *mm_open(const char *title, double width, double height, int hidden);
const char *mm_last_error(MmContext *);
int mm_poll(MmContext *, MmEvent *, double wait_ms);
void mm_size(MmContext *, double *width, double *height, double *scale);
void mm_begin(MmContext *, const float clear[4]);
void mm_quads(MmContext *, const MmQuad *, size_t count);
double mm_text(MmContext *, const uint8_t *, size_t len, double x, double y,
               double size, const float color[4], const float clip[4], int monospace);
double mm_text_width(MmContext *, const uint8_t *, size_t len, double size, int monospace);
int mm_present(MmContext *, MmFrameStats *);
int mm_capture(MmContext *, const char *png_path);
void mm_text_state(MmContext *, const uint8_t *, size_t len,
                   uint64_t selection_start, uint64_t selection_len,
                   const double caret_rect[4]);
void mm_cancel_composition(MmContext *);
void mm_cursor(MmContext *, uint32_t cursor);
void mm_title(MmContext *, const char *title);
const char *mm_clipboard(MmContext *);
void mm_set_clipboard(MmContext *, const char *text);
void mm_accessibility(MmContext *, const MmAccessibility *, size_t count);
void mm_close(MmContext *);
#endif
