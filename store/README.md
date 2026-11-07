# Store assets

What the Play console takes, and why each file looks the way it does.

| file | requirement | note |
|---|---|---|
| `icon-512.png` | 512 by 512, 24-bit PNG, no alpha | the launcher foreground flattened onto the app's own background (`#0A0A0A`), because Play rejects transparency |
| `feature-1024x500.png` | 1024 by 500, no alpha | the mark and its 180-degree copy (which reads as its own name, as in the README) beside the wordmark |
| `01-launcher.png` … `04-landscape.png` | 320 to 3840 px a side, aspect ratio at most 2:1 | the README screenshots with their sides padded to the ceiling; the pad is the app's background, so it is invisible |

Rebuild with the script in the audit's Appendix A, or by hand:

    python3 - <<'PY'
    from PIL import Image
    bg = (10, 10, 10)
    icon = Image.open("docs/media/app-icon.png").convert("RGBA")
    flat = Image.new("RGB", icon.size, bg)
    flat.paste(icon, (0, 0), icon)
    flat.save("store/icon-512.png")
    PY

The screenshots are taken on the device and live in `docs/media/`; `store/` holds the padded copies
Play accepts. The privacy policy text is `docs/PRIVACY.md`, published at `https://ldmux.io/privacy`.
