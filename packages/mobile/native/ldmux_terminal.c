/*
 * ldmux_terminal.c — the terminal core: drives libghostty-vt and encodes frames.
 *
 * No platform belongs in this file. It does not include `jni.h` and does not
 * know it is on Android, which is the point: `ldmux_jni.c` is one binding over
 * this, and the frame format is described in `ldmux_terminal.h`.
 */
#include "ldmux_terminal.h"

#include <stdlib.h>
#include <string.h>

#include <ghostty/vt.h>

struct LdmuxTerminal {
  GhosttyTerminal terminal;
  GhosttyRenderState render_state;
  GhosttyRenderStateRowIterator row_iter;
  GhosttyRenderStateRowCells cells;
  uint8_t *frame; /* growable output buffer, owned by this struct */
  size_t frame_cap;
  uint8_t *clip; /* last clipboard write, owned by this struct */
  size_t clip_cap;
  size_t clip_len;
  int clip_kind; /* one of the LDMUX_CLIPBOARD_* values */
  GhosttySelectionGesture gesture;
  uint8_t *sel; /* last formatted selection, owned by this struct */
  size_t sel_cap;
  size_t sel_len;
  /* Geometry, in surface pixels. Zero until the client says otherwise, which
   * makes every pointer-distance decision degenerate but never wrong. */
  uint32_t geo_columns;
  uint32_t geo_cell_w;
  uint32_t geo_cell_h;
  uint32_t geo_pad_left;
  uint32_t geo_screen_h;
  /* The grid as last rendered. A viewport coordinate is only meaningful against
   * a size, and this is the one the client is looking at. */
  uint16_t grid_cols;
  uint16_t grid_rows;
};

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

static void wr32(uint8_t *p, uint32_t v) {
  p[0] = (uint8_t)(v & 0xff);
  p[1] = (uint8_t)((v >> 8) & 0xff);
  p[2] = (uint8_t)((v >> 16) & 0xff);
  p[3] = (uint8_t)((v >> 24) & 0xff);
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

static bool ensure_clip(LdmuxTerminal *t, size_t need) {
  if (t->clip_cap >= need) return true;
  size_t cap = t->clip_cap ? t->clip_cap : 256;
  while (cap < need) cap *= 2;
  uint8_t *grown = (uint8_t *)realloc(t->clip, cap);
  if (!grown) return false;
  t->clip = grown;
  t->clip_cap = cap;
  return true;
}

/*
 * Clipboard writes.
 *
 * The engine hands us a *request* rather than performing the write, because it
 * cannot know what the platform's clipboard is. Answering it is not optional:
 * calling `reply` is what tells the engine the write happened, and returning
 * without replying denies it. So this takes the text, keeps it, and replies from
 * inside itself, which is the only place the contract allows.
 *
 * The text is kept here rather than passed straight on to Java because this file
 * is the portable half and has no JNI by design, and because the engine's
 * contract is synchronous while an Android clipboard write cannot be assumed to
 * be. "Taken" is the honest answer to the engine's question — the value is in our
 * hands, and the drain happens on the same thread that fed the bytes, which is
 * the very next thing either way.
 */
static void on_clipboard_write(GhosttyTerminal terminal, void *userdata,
                               const GhosttyClipboardWrite *write) {
  (void)terminal;
  LdmuxTerminal *t = (LdmuxTerminal *)userdata;
  int result = GHOSTTY_CLIPBOARD_WRITE_RESULT_IO_ERROR;

  if (t && write) {
    if (write->contents_len == 0) {
      /* The protocol's way of saying "clear the clipboard". A representation
       * whose data is empty is a different thing: it sets an empty value. */
      t->clip_kind = LDMUX_CLIPBOARD_CLEAR;
      t->clip_len = 0;
      result = GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS;
    } else {
      /* Every entry is a representation of the same value. Prefer plain text,
       * because that is all this platform's clipboard holds: a program offering
       * HTML, an image and text wants whichever we can actually honour, not
       * necessarily the first one it listed. */
      const GhosttyClipboardContent *pick = NULL;
      for (size_t i = 0; i < write->contents_len; i++) {
        const GhosttyClipboardContent *c = &write->contents[i];
        if (pick == NULL) pick = c;
        if (c->mime.len >= 10 && memcmp(c->mime.ptr, "text/plain", 10) == 0) {
          pick = c;
          break;
        }
      }
      if (pick && (pick->data.len == 0 || ensure_clip(t, pick->data.len))) {
        if (pick->data.len > 0) memcpy(t->clip, pick->data.ptr, pick->data.len);
        t->clip_len = pick->data.len;
        t->clip_kind = LDMUX_CLIPBOARD_SET;
        result = GHOSTTY_CLIPBOARD_WRITE_RESULT_SUCCESS;
      }
    }
  }

  GhosttyClipboardWriteReply reply;
  memset(&reply, 0, sizeof(reply));
  reply.size = sizeof(reply);
  reply.result = (GhosttyClipboardWriteResult)result;
  reply.remember = false;
  /* Unconditional: the engine ignores a second reply, and a request that arrived
   * with no reply function is nothing to crash over. */
  if (write && write->reply) write->reply(write, &reply);
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

LdmuxTerminal *ldmux_terminal_new(uint16_t cols, uint16_t rows) {
  LdmuxTerminal *t = (LdmuxTerminal *)calloc(1, sizeof(LdmuxTerminal));
  if (!t) return NULL;
  bool ok = ghostty_terminal_new(NULL, &t->terminal, cols, rows) == GHOSTTY_SUCCESS &&
            ghostty_render_state_new(NULL, &t->render_state) == GHOSTTY_SUCCESS &&
            ghostty_render_state_row_iterator_new(NULL, &t->row_iter) == GHOSTTY_SUCCESS &&
            ghostty_render_state_row_cells_new(NULL, &t->cells) == GHOSTTY_SUCCESS;
  if (!ok) {
    if (t->cells) ghostty_render_state_row_cells_free(t->cells);
    if (t->row_iter) ghostty_render_state_row_iterator_free(t->row_iter);
    if (t->render_state) ghostty_render_state_free(t->render_state);
    if (t->terminal) ghostty_terminal_free(t->terminal);
    free(t);
    return NULL;
  }
  if (ghostty_selection_gesture_new(NULL, &t->gesture) != GHOSTTY_SUCCESS) {
    t->gesture = NULL; /* selection stays unavailable; nothing else is affected */
  }

  /* The engine has to be able to find this struct from inside the callback, and
   * the callback is the only way a clipboard write becomes anything at all.
   * Without these, OSC 52 parses and is then discarded in silence: nothing
   * receives the value and the program is told nothing went wrong. */
  ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, t);
  ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE,
                       (const void *)on_clipboard_write);
  /* GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ is deliberately left unset: enabling it
   * would let any program running in the shell read the user's clipboard, which
   * is a capability this app was never asked for. */
  return t;
}

int ldmux_terminal_take_clipboard(LdmuxTerminal *t, const uint8_t **out,
                                  size_t *len) {
  if (out) *out = NULL;
  if (len) *len = 0;
  if (!t || t->clip_kind == LDMUX_CLIPBOARD_NONE) return LDMUX_CLIPBOARD_NONE;
  int kind = t->clip_kind;
  t->clip_kind = LDMUX_CLIPBOARD_NONE;
  if (kind == LDMUX_CLIPBOARD_SET) {
    if (out) *out = t->clip;
    if (len) *len = t->clip_len;
  }
  return kind;
}

static bool ensure_sel(LdmuxTerminal *t, size_t need) {
  if (t->sel_cap >= need) return true;
  size_t cap = t->sel_cap ? t->sel_cap : 4096;
  while (cap < need) cap *= 2;
  uint8_t *next = (uint8_t *)realloc(t->sel, cap);
  if (!next) return false;
  t->sel = next;
  t->sel_cap = cap;
  return true;
}

static GhosttyPoint viewport_point(uint32_t col, uint32_t row);
static void ensure_screen(LdmuxTerminal *t);

/*
 * Render once if there is no screen yet.
 *
 * A viewport coordinate is resolved by the engine against the screen as last
 * rendered, so a selection set before the first render resolves against nothing and
 * comes back empty — silently, which is the worst way for it to come back. Every
 * caller in the app has just drawn a frame, so in practice this never fires; it
 * exists so that "it did nothing" can never be the answer to a well-formed call.
 */
static void ensure_screen(LdmuxTerminal *t) {
  if (!t || !t->terminal || t->grid_rows) return;
  const uint8_t *buf = NULL;
  size_t len = 0;
  ldmux_terminal_render(t, &buf, &len);
}

/*
 * A grid reference for a viewport cell.
 *
 * The tag is VIEWPORT, not ACTIVE, and that is the whole point: everything the
 * client sends down and everything it draws is in the cells the user can see, so
 * the one place the two coordinate systems meet is here.
 */
static bool viewport_ref(LdmuxTerminal *t, int col, int row, GhosttyGridRef *out) {
  if (!t || !t->terminal || !out) return false;
  if (col < 0) col = 0;
  if (row < 0) row = 0;
  GhosttyPoint p = viewport_point((uint32_t)col, (uint32_t)row);
  return ghostty_terminal_grid_ref(t->terminal, p, out) == GHOSTTY_SUCCESS;
}

/*
 * Where a selection is, in viewport cells.
 *
 * A GhosttyGridRef carries a grid row *node* and an x, and the header does not
 * say whether its y is a viewport row or an absolute one in the scrollback. Rather
 * than guess, this finds the row by identity: a reference to column 0 of each
 * visible row is built and its node compared. That is exact under either
 * convention, costs one reference per visible row, and only runs when something is
 * actually selected.
 *
 * Answers false when there is nothing selected.
 */
static bool selection_endpoints(LdmuxTerminal *t, int *sc, int *sr, int *ec,
                                int *er) {
  if (!t || !t->terminal || !t->grid_rows) return false;
  GhosttySelection sel = GHOSTTY_INIT_SIZED(GhosttySelection);
  if (ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_SELECTION, &sel) !=
      GHOSTTY_SUCCESS)
    return false;
  if (!sel.start.node || !sel.end.node) return false;

  /* Which visible row is the selection on?
   *
   * A GhosttyGridRef carries a y that is measured from the top of the *grid*, not
   * from the top of the viewport — plain to see once rows exist above the viewport
   * and the same row answers 0 when it is on screen and 7 when seven rows of
   * scrollback sit above it. So subtracting is not needed and guessing is not
   * allowed: this asks the engine for the y of each visible row and compares the
   * engine's answer with the engine's answer. Both sides come from the same
   * projection, so the conversion cannot be wrong in either direction.
   *
   * The row's `node` looked like a cheaper identity to match on, and is not: it is
   * shared by whole runs of rows, so every endpoint matched the first row.
   */
  bool have_start = false, have_end = false;
  for (uint16_t row = 0; row < t->grid_rows; row++) {
    GhosttyGridRef probe = GHOSTTY_INIT_SIZED(GhosttyGridRef);
    if (!viewport_ref(t, 0, row, &probe)) continue;
    if (!have_start && probe.y == sel.start.y) {
      *sc = sel.start.x;
      *sr = row;
      have_start = true;
    }
    if (!have_end && probe.y == sel.end.y) {
      *ec = sel.end.x;
      *er = row;
      have_end = true;
    }
    if (have_start && have_end) break;
  }
  return have_start && have_end;
}

/*
 * Install a selection the gesture produced. The engine copies it immediately and
 * converts it to terminal-owned tracked state, so the snapshot's grid
 * references only have to be valid for the duration of this call — which they
 * are, because the gesture produced them from the current grid in the same
 * breath.
 */
static int install_selection(LdmuxTerminal *t, const GhosttySelection *sel) {
  if (!t || !t->terminal || !sel) return 0;
  GhosttySelection copy = *sel;
  copy.size = sizeof(GhosttySelection);
  return ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_SELECTION,
                              &copy) == GHOSTTY_SUCCESS;
}

static GhosttyPoint viewport_point(uint32_t col, uint32_t row) {
  GhosttyPoint p;
  memset(&p, 0, sizeof(p));
  p.tag = GHOSTTY_POINT_TAG_VIEWPORT;
  p.value.coordinate.x = (uint16_t)col;
  p.value.coordinate.y = row;
  return p;
}

/*
 * The one path every selection event takes. `have_cell` is what separates "the
 * pointer is on this cell" from "the pointer is off the grid", which the engine
 * records rather than guessing: a drag that leaves the terminal and comes back
 * must not silently re-anchor somewhere else.
 */
static int selection_event(LdmuxTerminal *t, GhosttySelectionGestureEventType type,
                           int have_cell, uint32_t col, uint32_t row,
                           uint64_t time_ns) {
  if (!t || !t->gesture || !t->terminal) return 0;
  ensure_screen(t);

  GhosttyGridRef ref = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  if (have_cell) {
    if (ghostty_terminal_grid_ref(t->terminal, viewport_point(col, row), &ref) !=
        GHOSTTY_SUCCESS) {
      have_cell = 0;
    }
  }

  GhosttySelectionGestureEvent ev = NULL;
  if (ghostty_selection_gesture_event_new(NULL, &ev, type) != GHOSTTY_SUCCESS) {
    return 0;
  }

  /* Options persist on a reused event, so every one that matters is set (or
   * deliberately cleared) on every call. The slop that decides whether two taps
   * are one word is generous on purpose: a finger is not a mouse, and a
   * double-tap that misses by a few pixels should still select the word. */
  ghostty_selection_gesture_event_set(
      ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REF, have_cell ? (const void *)&ref : NULL);
  if (have_cell) {
    GhosttySurfacePosition pos;
    pos.x = (double)(t->geo_pad_left + col * t->geo_cell_w) + (double)t->geo_cell_w / 2.0;
    pos.y = (double)(row * t->geo_cell_h) + (double)t->geo_cell_h / 2.0;
    ghostty_selection_gesture_event_set(
        ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION, &pos);
  } else {
    ghostty_selection_gesture_event_set(
        ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_POSITION, NULL);
  }
  ghostty_selection_gesture_event_set(ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_TIME_NS,
                                      &time_ns);
  double slop = (double)(t->geo_cell_w ? t->geo_cell_w : 24u) * 2.0;
  ghostty_selection_gesture_event_set(
      ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_REPEAT_DISTANCE, &slop);

  if (type == GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DRAG ||
      type == GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_AUTOSCROLL_TICK) {
    GhosttySelectionGestureGeometry geo;
    memset(&geo, 0, sizeof(geo));
    geo.columns = t->geo_columns ? t->geo_columns : 1;
    geo.cell_width = t->geo_cell_w ? t->geo_cell_w : 1;
    geo.padding_left = t->geo_pad_left;
    geo.screen_height = t->geo_screen_h ? t->geo_screen_h : 1;
    ghostty_selection_gesture_event_set(
        ev, GHOSTTY_SELECTION_GESTURE_EVENT_OPT_GEOMETRY, &geo);
  }

  GhosttySelection sel = GHOSTTY_INIT_SIZED(GhosttySelection);
  GhosttyResult r = ghostty_selection_gesture_event(t->gesture, t->terminal, ev, &sel);
  ghostty_selection_gesture_event_free(ev);

  /* A release updates the gesture but produces no selection, and a press whose
   * click count is still being decided can do the same. Both are successes, so
   * only a real failure is reported as one. */
  if (r == GHOSTTY_SUCCESS) return install_selection(t, &sel);
  if (r == GHOSTTY_NO_VALUE) return 0;
  return 0;
}

int ldmux_terminal_set_geometry(LdmuxTerminal *t, uint32_t columns, uint32_t cell_w,
                                uint32_t cell_h, uint32_t pad_left,
                                uint32_t screen_h) {
  if (!t) return 0;
  int changed = t->geo_columns != columns || t->geo_cell_w != cell_w ||
                t->geo_cell_h != cell_h || t->geo_pad_left != pad_left ||
                t->geo_screen_h != screen_h;
  t->geo_columns = columns;
  t->geo_cell_w = cell_w;
  t->geo_cell_h = cell_h;
  t->geo_pad_left = pad_left;
  t->geo_screen_h = screen_h;
  return changed;
}

int ldmux_terminal_select_press(LdmuxTerminal *t, uint32_t col, uint32_t row,
                                uint64_t time_ms) {
  return selection_event(t, GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_PRESS, 1, col, row,
                         time_ms * 1000000ull);
}

int ldmux_terminal_selection_set(LdmuxTerminal *t, int start_col, int start_row,
                                  int end_col, int end_row) {
  if (!t || !t->terminal) return 0;
  ensure_screen(t);
  /* The engine wants an ordered range; a finger does not know about order. This is
   * the one place that has to be true, because dragging the end handle back above
   * the start is an ordinary thing to do and an inverted selection formats to
   * nothing at all — a copy that silently produces an empty string. */
  if (start_row > end_row || (start_row == end_row && start_col > end_col)) {
    int c = start_col, r = start_row;
    start_col = end_col; start_row = end_row;
    end_col = c; end_row = r;
  }
  GhosttyGridRef a = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  GhosttyGridRef b = GHOSTTY_INIT_SIZED(GhosttyGridRef);
  if (!viewport_ref(t, start_col, start_row, &a)) return 0;
  if (!viewport_ref(t, end_col, end_row, &b)) return 0;
  GhosttySelection sel = GHOSTTY_INIT_SIZED(GhosttySelection);
  sel.start = a;
  sel.end = b;
  sel.rectangle = false;
  return install_selection(t, &sel);
}

int ldmux_terminal_selection_bounds(LdmuxTerminal *t, int *start_col, int *start_row,
                                    int *end_col, int *end_row) {
  if (!t || !t->terminal || !start_col || !start_row || !end_col || !end_row)
    return 0;
  ensure_screen(t);
  return selection_endpoints(t, start_col, start_row, end_col, end_row) ? 1 : 0;
}

int ldmux_terminal_select_deep_press(LdmuxTerminal *t, uint64_t time_ms) {
  return selection_event(t, GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DEEP_PRESS, 0, 0, 0,
                         time_ms * 1000000ull);
}

int ldmux_terminal_select_drag(LdmuxTerminal *t, uint32_t col, uint32_t row,
                               uint64_t time_ms) {
  return selection_event(t, GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_DRAG, 1, col, row,
                         time_ms * 1000000ull);
}

int ldmux_terminal_select_release(LdmuxTerminal *t, uint32_t col, uint32_t row,
                                  int valid_cell, uint64_t time_ms) {
  /* The release carries the cell it happened on, like every other event. It is
   * the last position the engine hears about, so reporting (0,0) here tells it
   * the finger travelled back to the first cell of the screen and the click it
   * was counting never happened. */
  return selection_event(t, GHOSTTY_SELECTION_GESTURE_EVENT_TYPE_RELEASE,
                         valid_cell != 0, col, row, time_ms * 1000000ull);
}

int ldmux_terminal_select_clear(LdmuxTerminal *t) {
  if (!t || !t->terminal) return 0;
  if (t->gesture) ghostty_selection_gesture_reset(t->gesture, t->terminal);
  ghostty_terminal_set(t->terminal, GHOSTTY_TERMINAL_OPT_SELECTION, NULL);
  return 1;
}

int ldmux_terminal_select_all(LdmuxTerminal *t) {
  if (!t || !t->terminal) return 0;
  GhosttySelection sel = GHOSTTY_INIT_SIZED(GhosttySelection);
  if (ghostty_terminal_select_all(t->terminal, &sel) != GHOSTTY_SUCCESS) {
    /* An empty screen has nothing to select, which is not a failure. */
    return ldmux_terminal_select_clear(t);
  }
  return install_selection(t, &sel);
}

const uint8_t *ldmux_terminal_selection_text(LdmuxTerminal *t, size_t *len) {
  if (len) *len = 0;
  if (!t || !t->terminal) return NULL;

  GhosttyTerminalSelectionFormatOptions opts =
      GHOSTTY_INIT_SIZED(GhosttyTerminalSelectionFormatOptions);
  opts.emit = GHOSTTY_FORMATTER_FORMAT_PLAIN;
  /* Unwrap and trim is what Ghostty's own Screen.selectionString() does, and it
   * is what copying out of a terminal should do: the wrap belongs to the window,
   * not to the text. */
  opts.unwrap = true;
  opts.trim = true;
  opts.selection = NULL; /* the terminal's current selection */

  /* The buffer contract for a short buffer is not documented tightly enough to
   * rely on, so this grows and retries instead of trusting a return code to
   * report a size: at worst the first attempt teaches us the size, and the loop
   * is bounded so a misbehaving engine cannot spin here. */
  for (int attempt = 0; attempt < 4; attempt++) {
    if (!t->sel && !ensure_sel(t, 4096)) return NULL;
    size_t written = 0;
    GhosttyResult r = ghostty_terminal_selection_format_buf(
        t->terminal, opts, t->sel, t->sel_cap, &written);
    if (r == GHOSTTY_SUCCESS) {
      /* An empty range formats to an empty string rather than to nothing, and
       * an empty string is not a selection: the caller would put "" on the
       * clipboard and call it a copy. */
      if (written == 0) return NULL;
      t->sel_len = written;
      if (len) *len = written;
      return t->sel;
    }
    if (r == GHOSTTY_NO_VALUE) return NULL; /* nothing selected */
    if (!ensure_sel(t, written > t->sel_cap ? written + 1 : t->sel_cap * 2)) return NULL;
  }
  return NULL;
}

void ldmux_terminal_free(LdmuxTerminal *t) {
  if (!t) return;
  /* Free the gesture against the living terminal so it can release the tracked
   * grid references it holds. Doing it after ghostty_terminal_free would leave
   * the gesture pointing at freed pages. */
  if (t->gesture) ghostty_selection_gesture_free(t->gesture, t->terminal);
  if (t->cells) ghostty_render_state_row_cells_free(t->cells);
  if (t->row_iter) ghostty_render_state_row_iterator_free(t->row_iter);
  if (t->render_state) ghostty_render_state_free(t->render_state);
  if (t->terminal) ghostty_terminal_free(t->terminal);
  free(t->frame);
  free(t->clip);
  free(t->sel);
  free(t);
}

void ldmux_terminal_write(LdmuxTerminal *t, const uint8_t *bytes, size_t len) {
  if (!t || !bytes || len == 0) return;
  ghostty_terminal_vt_write(t->terminal, bytes, len);
}

void ldmux_terminal_resize(LdmuxTerminal *t, uint16_t cols, uint16_t rows) {
  if (!t) return;
  ghostty_terminal_resize(t->terminal, cols, rows, 0, 0);
}

void ldmux_terminal_scroll(LdmuxTerminal *t, int32_t delta) {
  if (!t) return;
  GhosttyTerminalScrollViewport behavior;
  if (delta == LDMUX_SCROLL_TOP) {
    behavior.tag = GHOSTTY_SCROLL_VIEWPORT_TOP;
    behavior.value.delta = 0;
  } else if (delta == LDMUX_SCROLL_BOTTOM) {
    behavior.tag = GHOSTTY_SCROLL_VIEWPORT_BOTTOM;
    behavior.value.delta = 0;
  } else if (delta == 0) {
    return;
  } else {
    behavior.tag = GHOSTTY_SCROLL_VIEWPORT_DELTA;
    behavior.value.delta = (intptr_t)delta;
  }
  ghostty_terminal_scroll_viewport(t->terminal, behavior);
}

bool ldmux_terminal_render(LdmuxTerminal *t, const uint8_t **out, size_t *len) {
  if (!t || !out || !len) return false;
  if (ghostty_render_state_update(t->render_state, t->terminal) != GHOSTTY_SUCCESS)
    return false;

  /* Row data lives inside the render state; the iterator is populated from it
   * and is only valid until the next update (see render.h). */
  if (ghostty_render_state_get(t->render_state,
                               GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
                               &t->row_iter) != GHOSTTY_SUCCESS)
    return false;

  uint16_t cols = 0, rows = 0;
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_COLS, &cols);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_ROWS, &rows);

  t->grid_cols = cols;
  t->grid_rows = rows;

  GhosttyRenderStateColors colors = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_COLORS,
                           &colors);
  GhosttyRenderStateCursor cursor = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
  ghostty_render_state_get(t->render_state, GHOSTTY_RENDER_STATE_DATA_CURSOR,
                           &cursor);

  size_t need = FRAME_HEADER + (size_t)cols * (size_t)rows * CELL_RECORD_MAX;
  if (!ensure_frame(t, need)) return false;

  uint8_t *buf = t->frame;
  wr16(buf + 0, cols);
  wr16(buf + 2, rows);
  wr16(buf + 4, cursor.viewport_has_value ? cursor.viewport_x : 0);
  wr16(buf + 6, cursor.viewport_has_value ? cursor.viewport_y : 0);
  /* The program's own request, reported as asked. Whether the coordinates are
   * *real* is a separate fact and travels in its own byte (`cursorKnown`, byte
   * 40): `visible` is "draw a cursor here", `viewport_has_value` is "the emulator
   * knows where it is". A client needs both to draw one and only the second to
   * move one, and `pi` is exactly the program that separates them: it hides the
   * hardware cursor and draws its own inverse-video cell instead. */
  buf[8] = (uint8_t)(cursor.visible ? 1 : 0);
  buf[9] = (uint8_t)cursor.visual_style;

  size_t p = FRAME_HEADER;
  uint8_t def_fg[3] = {colors.foreground.r, colors.foreground.g, colors.foreground.b};
  uint8_t def_bg[3] = {colors.background.r, colors.background.g, colors.background.b};
  buf[10] = def_fg[0]; buf[11] = def_fg[1]; buf[12] = def_fg[2];
  buf[13] = def_bg[0]; buf[14] = def_bg[1]; buf[15] = def_bg[2];

  /* Whether the cursor sits at a password prompt. This is the signal that lets
   * a client decide it must NOT echo input locally: the emulator already knows,
   * so nothing has to guess. */
  bool password_input = false;
  ghostty_render_state_get(t->render_state,
                           GHOSTTY_RENDER_STATE_DATA_CURSOR_PASSWORD_INPUT,
                           &password_input);
  buf[16] = (uint8_t)(password_input ? 1 : 0);

  /* Scroll position. There is deliberately no change notification for this
   * (the header says to poll and diff per frame), which is exactly what a
   * render-state read does. */
  GhosttyTerminalScrollbar sb = {0, 0, 0};
  ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_SCROLLBAR, &sb);
  bool viewport_active = false;
  ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_VIEWPORT_ACTIVE,
                       &viewport_active);
  buf[17] = (uint8_t)(viewport_active ? 1 : 0);

  /* Mouse modes, read from the emulator rather than from a server-side poll.
   *
   * This has to be local: a program can enable mouse reporting at any moment
   * (`vim` on startup, `less` on a keypress), and the client only learns the
   * server's view of the pane when a snapshot arrives. Waiting for one meant a
   * program that had just asked for the mouse still got cursor keys, so taps
   * scrolled its view instead of clicking. The emulator saw the escape sequence,
   * so it is right on the very frame that follows it. */
  bool mouse_tracking = false;
  ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING,
                       &mouse_tracking);
  buf[18] = (uint8_t)(mouse_tracking ? 1 : 0);

  wr32(buf + 19, (uint32_t)sb.total);
  wr32(buf + 23, (uint32_t)sb.offset);
  wr32(buf + 27, (uint32_t)sb.len);

  /* The alternate screen, read from the emulator for the same reason as the
   * mouse modes above: it changes the instant a program starts, and the client
   * needs it on that frame. It separates the two kinds of full-screen program a
   * person can be running. `pi` and Claude Code draw their own composer on the
   * PRIMARY screen and want arrow keys; `vim`, `less`, `htop` and `nano` take
   * the alternate screen, where an arrow key is a command rather than "put the
   * cursor here". Only the first kind should have taps moved into it. */
  GhosttyTerminalScreen active_screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
  ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN,
                       &active_screen);
  buf[32] = (uint8_t)(active_screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE ? 1 : 0);

  /* Bytes 33..39 were reserved and now carry the selection, so the client can draw
   * a handle at each end without asking a second time and without a second
   * coordinate system of its own. The flag comes first: a client that predates
   * this reads a zero and draws nothing, which is the whole reason for spending
   * reserved bytes instead of a version bump. */
  buf[33] = 0;
  buf[34] = 0; buf[35] = 0; buf[36] = 0;
  buf[37] = 0; buf[38] = 0; buf[39] = 0;
  {
    int sc = 0, sr = 0, ec = 0, er = 0;
    if (selection_endpoints(t, &sc, &sr, &ec, &er)) {
      buf[33] = 1;
      buf[34] = (uint8_t)(sc & 0xFF); buf[35] = (uint8_t)((sc >> 8) & 0xFF);
      buf[36] = (uint8_t)(sr & 0xFF);
      buf[37] = (uint8_t)(ec & 0xFF); buf[38] = (uint8_t)((ec >> 8) & 0xFF);
      buf[39] = (uint8_t)(er & 0xFF);
    }
  }

  /* SGR encoding (1006) specifically: it is the only encoding we synthesize,
   * because the older forms cap coordinates at 223 columns. */
  GhosttyTerminalModeConfig sgr_mouse = {0};
  sgr_mouse.mode = GHOSTTY_MODE_SGR_MOUSE;
  bool sgr_mouse_on = false;
  if (ghostty_terminal_get(t->terminal, GHOSTTY_TERMINAL_DATA_MODE, &sgr_mouse) ==
      GHOSTTY_SUCCESS) {
    sgr_mouse_on = sgr_mouse.value;
  }
  buf[31] = (uint8_t)(sgr_mouse_on ? 1 : 0);

  /* Whether cursorX/cursorY above are meaningful. The emulator's own words: "when
   * viewport_has_value is false, viewport_x, viewport_y and wide_tail contain
   * undefined data and must not be read" — which is why the coordinates are
   * written as 0 in that case, and why the position alone cannot tell a client
   * whether it is real. Tap-to-cursor measures every step FROM the cursor, so it
   * has to know: walking from a fabricated (0,0) aims a key run at the wrong
   * cell. Bytes 41..47 are reserved, so the next header field costs nothing. */
  buf[40] = (uint8_t)(cursor.viewport_has_value ? 1 : 0);
  buf[41] = 0; buf[42] = 0; buf[43] = 0; buf[44] = 0;
  buf[45] = 0; buf[46] = 0; buf[47] = 0;
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
        buf[p++] = 0; buf[p++] = 0; buf[p++] = 0;
        buf[p++] = def_fg[0]; buf[p++] = def_fg[1]; buf[p++] = def_fg[2];
        buf[p++] = def_bg[0]; buf[p++] = def_bg[1]; buf[p++] = def_bg[2];
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

      /* Selection rides in the cell's own flag word rather than a new header
       * field. Bit 9 was the first free bit, the client already decodes this
       * word for every cell it draws, and a selection is a property of cells
       * rather than of the frame — so the wire format does not change at all
       * for this feature, and an older client simply ignores the bit. */
      bool selected = false;
      if (ghostty_render_state_row_cells_get(
              t->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED,
              &selected) == GHOSTTY_SUCCESS && selected) {
        flags |= 1u << 9;
      }

      buf[p++] = (uint8_t)n;
      buf[p++] = (uint8_t)(flags & 0xff);
      buf[p++] = (uint8_t)((flags >> 8) & 0xff);
      buf[p++] = fg.r; buf[p++] = fg.g; buf[p++] = fg.b;
      buf[p++] = bg.r; buf[p++] = bg.g; buf[p++] = bg.b;
      if (n) { memcpy(buf + p, utf8, n); p += n; }
    }
    have_row = ghostty_render_state_row_iterator_next(t->row_iter);
  }

  *out = t->frame;
  *len = p;
  return true;
}

void ldmux_terminal_clean(LdmuxTerminal *t) {
  if (!t) return;
  ghostty_render_state_clean(t->render_state);
}
