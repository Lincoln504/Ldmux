# The README mark

`app-icon.png` is the launcher foreground (`packages/mobile/composeApp/src/androidMain/res/drawable/ic_launcher_foreground.xml`)
drawn on a `#0A0A0A` rounded rectangle with a 21.5 % corner radius, which is slightly tighter than the
launcher mask. `app-icon-pair.png` is that image with a copy rotated 180 degrees — flipped both vertically and
sideways — beneath it, 588 px apart on a 512 x 1100 canvas. The rotated copy reads `pl`, so the pair
reads as the name.

Rebuild with ImageMagick, from a 512 px render of the icon:

    convert app-icon.png -rotate 180 turned.png
    convert -size 512x1100 xc:none app-icon.png -geometry +0+0 -composite \
            turned.png -geometry +0+588 -composite app-icon-pair.png

Both are referenced from `README.md` only.
