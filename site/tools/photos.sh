#!/bin/sh
# Product photos for the website, from the studio set in
# ../ref/OpenWalkieTalkie (黑绿 = black/green, 白橙 = white/orange,
# 双 = both together; November 2026, the redesigned body). Photos are
# auto-oriented, cropped to the window the page shows (4:5 for the hero, 3:4
# for the hardware stack, 3:2 for the colourway cards), resized for a 2x
# display, stripped of all metadata and written as WebP.
#
#   sh tools/photos.sh      # → public/assets/photos/*.webp
#
# Crops are hand-picked per photo so the device sits where the card expects
# it. Needs ImageMagick 7 (brew install imagemagick).
set -eu
cd "$(dirname "$0")/.."
SRC=../ref/OpenWalkieTalkie

shot() { # name source crop(WxH+X+Y) size
  magick "$SRC/$2" -auto-orient -crop "$3" +repage -resize "$4" -strip -quality 82 -define webp:method=6 "public/assets/photos/$1.webp"
  printf '%s  ' "public/assets/photos/$1.webp"; magick identify -format '%wx%h\n' "public/assets/photos/$1.webp"
}

# hero: white/orange in front, black/green out of focus behind (4:5)
shot hero-pair      双/8.jpg   2016x2520+0+520    1045x1306
# hardware stack (3:4), in the order the page shows them
shot fully-open     黑绿/4.jpg 2160x2880+0+200    750x1000
shot black-front    黑绿/7.jpg 1620x2160+270+570  750x1000
shot white-front    白橙/17.jpg 1620x2160+230+880 750x1000
shot faces          白橙/12.jpg 1620x2160+330+700 750x1000
shot pair-top       双/7.jpg   2160x2880+0+200    750x1000
shot white-top      白橙/16.jpg 2160x2880+0+180   750x1000
# colourway cards (3:2)
shot black-green    黑绿/13.jpg 2700x1800+225+216 1536x1024
shot white-orange   白橙/8.jpg  2700x1800+270+230 1536x1024
