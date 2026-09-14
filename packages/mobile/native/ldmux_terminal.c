/*
 * ldmux_terminal.c — JNI bridge from Kotlin to libghostty-vt (the Ghostty
 * terminal core). One narrow seam: Kotlin creates a terminal, feeds it the
 * remote PTY's bytes, and reads back a self-describing frame of styled cells.
 *
 * libghostty-vt's C API is marked unstable (0.1.0-dev); every symbol we use is
 * confined to this file, so an upstream change breaks one translation unit, not
 * the whole app.
 *
 * Frame format returned by nativeRender (little-endian):
 *   header (10 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle, u8 reserved0, u8 reserved1
 *   then cols*rows cell records, row-major:
 *                      u8 utf8Len, u8 flagsLo, u8 flagsHi,
 *                      u8 fgR, u8 fgG, u8 fgB, u8 bgR, u8 bgG, u8 bgB,
 *                      u8 utf8[utf8Len]
 *   flags: bit0 bold, bit1 italic, bit2 underline, bit3 faint, bit4 blink,
 *          bit5 inverse, bit6 invisible, bit7 strikethrough, bit8 overline
 */
#include <jni.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <ghostty/vt.h>

#define CELL_MAX_UTF8 16
#define CELL_RECORD_MAX (9 + CELL_MAX_UTF8)
#define FRAME_HEADER 10

typedef struct {
  GhosttyTerminal terminal;
  GhosttyRenderState render_state;
  GhosttyRenderStateRowIterator row_iter;
  GhosttyRenderStateRowCells cells;
  uint8_t *frame; /* growable output buffer */
  size_t frame_cap;
} LdmuxTerminal;

static GhosttyColorRgb resolve_color(GhosttyStyleColor color,
                                     const GhosttyRenderStateColors *colors,
                                     GhosttyColorRgb fallback) {
  switch (color.tag) {
    case GHOSTTY_STYLE_COLOR_RGB:
      return color.value.rgb;
    case GHOSTTY_STYLE_COLOR_PALETTE:
      return colors->palette[color.value.palette];
    default:
      return fallback;
  }
}

static void wr16(uint8_t *p, uint16_t v) {
  p[0] = (uint8_t)(v & 0xff);
  p[1] = (uint8_t)((v >> 8) & 0xff);
}

static bool ensure_frame(LdmuxTerminal *t, size_t need) {
  if (t->frame_cap >= need) return true;
  size_t cap = t->frame_cap ? t->frame_cap : 4096;
  while (cap < need) cap *= 2;
  uint8_t *grown = (uint8_t *)realloc(t->frame, cap);
  if (!grown) return false;
  t->frame = grown;
  t->frame_cap = cap;
  return true;
}

/* Encode one Unicode codepoint as UTF-8. Returns the byte count. */
static size_t utf8_encode(uint32_t cp, uint8_t *out) {
  if (cp < 0x80) {
    out[0] = (uint8_t)cp;
    return 1;
  } else if (cp < 0x800) {
    out[0] = (uint8_t)(0xC0 | (cp >> 6));
    out[1] = (uint8_t)(0x80 | (cp & 0x3F));
    return 2;
  } else if (cp < 0x10000) {
    out[0] = (uint8_t)(0xE0 | (cp >> 12));
    out[1] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
    out[2] = (uint8_t)(0x80 | (cp & 0x3F));
    return 3;
  }
  out[0] = (uint8_t)(0xF0 | (cp >> 18));
  out[1] = (uint8_t)(0x80 | ((cp >> 12) & 0x3F));
  out[2] = (uint8_t)(0x80 | ((cp >> 6) & 0x3F));
  out[3] = (uint8_t)(0x80 | (cp & 0x3F));
  return 4;
}

JNIEXPORT jlong JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_create(JNIEnv *env, jclass cls,
                                                 jint cols, jint rows) {
  (void)env; (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)calloc(1, sizeof(LdmuxTerminal));
  if (!t) return 0;
  bool ok = ghostty_terminal_new(NULL, &t->terminal, (uint16_t)cols,
                                 (uint16_t)rows) == GHOSTTY_SUCCESS &&
            ghostty_render_state_new(NULL, &t->render_state) == GHOSTTY_SUCCESS &&
            ghostty_render_state_row_iterator_new(NULL, &t->row_iter) == GHOSTTY_SUCCESS &&
            ghostty_render_state_row_cells_new(NULL, &t->cells) == GHOSTTY_SUCCESS;
  if (!ok) {
    if (t->cells) ghostty_render_state_row_cells_free(t->cells);
    if (t->row_iter) ghostty_render_state_row_iterator_free(t->row_iter);
    if (t->render_state) ghostty_render_state_free(t->render_state);
    if (t->terminal) ghostty_terminal_free(t->terminal);
    free(t);
    return 0;
  }
  return (jlong)(intptr_t)t;
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_destroy(JNIEnv *env, jclass cls,
                                                  jlong handle) {
  (void)env; (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)(intptr_t)handle;
  if (!t) return;
  if (t->cells) ghostty_render_state_row_cells_free(t->cells);
  if (t->row_iter) ghostty_render_state_row_iterator_free(t->row_iter);
  if (t->render_state) ghostty_render_state_free(t->render_state);
  if (t->terminal) ghostty_terminal_free(t->terminal);
  free(t->frame);
  free(t);
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_write(JNIEnv *env, jclass cls,
                                                jlong handle, jbyteArray data,
                                                jint len) {
  (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)(intptr_t)handle;
  if (!t || !data || len <= 0) return;
  jbyte *buf = (*env)->GetByteArrayElements(env, data, NULL);
  if (!buf) return;
  ghostty_terminal_vt_write(t->terminal, (const uint8_t *)buf, (size_t)len);
  (*env)->ReleaseByteArrayElements(env, data, buf, JNI_ABORT);
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_resize(JNIEnv *env, jclass cls,
                                                 jlong handle, jint cols,
                                                 jint rows) {
  (void)env; (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)(intptr_t)handle;
  if (!t) return;
  ghostty_terminal_resize(t->terminal, (uint16_t)cols, (uint16_t)rows, 0, 0);
}

JNIEXPORT jbyteArray JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_render(JNIEnv *env, jclass cls,
                                                 jlong handle) {
  (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)(intptr_t)handle;
  if (!t) return NULL;
  if (ghostty_render_state_update(t->render_state, t->terminal) != GHOSTTY_SUCCESS)
    return NULL;

  /* Row data lives inside the render state; the iterator is populated from it
   * and is only valid until the next update (see render.h). */
  if (ghostty_render_state_get(t->render_state,
                               GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                               &t->row_iter) != GHOSTTY_SUCCESS)
    return NULL;

  uint16_t cols = 0, rows = 0;
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_COLS, &cols);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows);

  GhosttyRenderStateColors colors = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_COLORS,
                           &colors);
  GhosttyRenderStateCursor cursor = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_CURSOR,
                           &cursor);

  size_t need = FRAME_HEADER + (size_t)cols * (size_t)rows * CELL_RECORD_MAX;
  if (!ensure_frame(t, need)) return NULL;

  uint8_t *out = t->frame;
  wr16(out + 0, cols);
  wr16(out + 2, rows);
  wr16(out + 4, cursor.viewport_has_value ? cursor.viewport_x : 0);
  wr16(out + 6, cursor.viewport_has_value ? cursor.viewport_y : 0);
  out[8] = (uint8_t)(cursor.visible ? 1 : 0);
  out[9] = (uint8_t)cursor.visual_style;

  size_t p = FRAME_HEADER;
  uint8_t def_fg[3] = {colors.foreground.r, colors.foreground.g, colors.foreground.b};
  uint8_t def_bg[3] = {colors.background.r, colors.background.g, colors.background.b};
  /* The row/cell iterators may skip trailing blank cells, so a fixed cols*rows
   * encoder is mandatory: pad every missing cell with a blank. */
  bool have_row = ghostty_render_state_row_iterator_next(t->row_iter);
  for (int row = 0; row < rows; row++) {
    bool have_cells = false;
    if (have_row) {
      have_cells = ghostty_render_state_row_get(
                       t->row_iter, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                       &t->cells) == GHOSTTY_SUCCESS;
    }
    for (int col = 0; col < cols; col++) {
      if (p + CELL_RECORD_MAX > t->frame_cap) break;
      if (!have_cells || !ghostty_render_state_row_cells_next(t->cells)) {
        out[p++] = 0; out[p++] = 0; out[p++] = 0;
        out[p++] = def_fg[0]; out[p++] = def_fg[1]; out[p++] = def_fg[2];
        out[p++] = def_bg[0]; out[p++] = def_bg[1]; out[p++] = def_bg[2];
        continue;
      }

      uint32_t glen = 0;
      ghostty_render_state_row_cells_get(
          t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN, &glen);

      uint8_t utf8[CELL_MAX_UTF8];
      size_t n = 0;
      if (glen > 0) {
        uint32_t cps[32];
        uint32_t want = glen < 32 ? glen : 32;
        ghostty_render_state_row_cells_get(
            t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, cps);
        for (uint32_t i = 0; i < want && n < CELL_MAX_UTF8;) {
          uint8_t tmp[4];
          size_t w = utf8_encode(cps[i], tmp);
          if (n + w > CELL_MAX_UTF8) break;
          memcpy(utf8 + n, tmp, w);
          n += w;
          i++;
        }
      }

      GhosttyStyle style = GHOSTTY_INIT_SIZED(GhosttyStyle);
      ghostty_render_state_row_cells_get(
          t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style);
      GhosttyColorRgb fg = resolve_color(style.fg_color, &colors, colors.foreground);
      GhosttyColorRgb bg = resolve_color(style.bg_color, &colors, colors.background);

      uint16_t flags = 0;
      if (style.bold) flags |= 1u << 0;
      if (style.italic) flags |= 1u << 1;
      if (style.underline) flags |= 1u << 2;
      if (style.faint) flags |= 1u << 3;
      if (style.blink) flags |= 1u << 4;
      if (style.inverse) flags |= 1u << 5;
      if (style.invisible) flags |= 1u << 6;
      if (style.strikethrough) flags |= 1u << 7;
      if (style.overline) flags |= 1u << 8;

      out[p++] = (uint8_t)n;
      out[p++] = (uint8_t)(flags & 0xff);
      out[p++] = (uint8_t)((flags >> 8) & 0xff);
      out[p++] = fg.r; out[p++] = fg.g; out[p++] = fg.b;
      out[p++] = bg.r; out[p++] = bg.g; out[p++] = bg.b;
      if (n) { memcpy(out + p, utf8, n); p += n; }
    }
    have_row = ghostty_render_state_row_iterator_next(t->row_iter);
  }

  jbyteArray result = (*env)->NewByteArray(env, (jsize)p);
  if (!result) return NULL;
  (*env)->SetByteArrayRegion(env, result, 0, (jsize)p, (const jbyte *)out);
  return result;
}

JNIEXPORT void JNICALL
Java_dev_ldmux_mobile_term_NativeTerminal_clean(JNIEnv *env, jclass cls,
                                                jlong handle) {
  (void)env; (void)cls;
  LdmuxTerminal *t = (LdmuxTerminal *)(intptr_t)handle;
  if (!t) return;
  ghostty_render_state_clean(t->render_state);
}