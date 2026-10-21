# One door, drawn once, written as SVG (previews, PNGs) and Android vectors.
BG_FROM, BG_TO, GOLD = '#0E1220', '#262E47', '#E8BE62'
def door(s, cx, cy):
    """Path data for the door at scale s, centred on (cx, cy). The glyph's
    own box is x 24..76, y 21..80 (centre 50, 50.5)."""
    X=lambda x: f'{cx+(x-50)*s:.3f}'; Y=lambda y: f'{cy+(y-50.5)*s:.3f}'; R=lambda r: f'{r*s:.3f}'
    outer=f'M{X(24)},{Y(80)} L{X(24)},{Y(47)} A{R(26)},{R(26)} 0 0,1 {X(76)},{Y(47)} L{X(76)},{Y(80)} Z'
    inner=f'M{X(38.5)},{Y(80)} L{X(38.5)},{Y(51.5)} A{R(11.5)},{R(11.5)} 0 0,1 {X(61.5)},{Y(51.5)} L{X(61.5)},{Y(80)} Z'
    # The keyhole of light in the doorway.
    kc=f'M{X(50-4.6)},{Y(60)} A{R(4.6)},{R(4.6)} 0 1,1 {X(50+4.6)},{Y(60)} A{R(4.6)},{R(4.6)} 0 1,1 {X(50-4.6)},{Y(60)} Z'
    kb=f'M{X(52.4)},{Y(62)} L{X(54)},{Y(72)} L{X(46)},{Y(72)} L{X(47.6)},{Y(62)} Z'
    return outer+' '+inner, kc+' '+kb

def svg(size, shape='squircle', mono=False):
    rx={'squircle':23,'circle':50,'square':0}[shape]
    frame, key = door(0.92, 50, 50)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" viewBox="0 0 100 100">'
            f'<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="{BG_FROM}"/><stop offset="1" stop-color="{BG_TO}"/></linearGradient></defs>'
            f'<rect width="100" height="100" rx="{rx}" fill="url(#g)"/>'
            f'<path fill-rule="evenodd" d="{frame}" fill="{GOLD}"/><path d="{key}" fill="{GOLD}"/></svg>')
open('icon.svg','w').write(svg(512,'square'))
open('icon_round.svg','w').write(svg(512,'circle'))

# Android adaptive icon: 108dp canvas, keep the art inside the middle ~60dp.
fr, ky = door(0.80, 54, 54)
VEC='''<?xml version="1.0" encoding="utf-8"?>
<!-- {what} Generated from one drawing (the Sanctuary door). -->
<vector xmlns:android="http://schemas.android.com/apk/res/android"{extra}
    android:width="{w}dp"
    android:height="{w}dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
{body}</vector>
'''
def p(d,color,evenodd=False):
    return (f'    <path\n        android:fillColor="{color}"\n'+('        android:fillType="evenOdd"\n' if evenodd else '')+f'        android:pathData="{d}" />\n')
open('ic_launcher_foreground.xml','w').write(VEC.format(what='App icon: the gold door.',extra='',w=108,body=p(fr,'#FFE8BE62',True)+p(ky,'#FFE8BE62')))
open('ic_launcher_monochrome.xml','w').write(VEC.format(what='Themed app icon (Android 13+): the door in one colour.',extra='',w=108,body=p(fr,'#FFFFFFFF',True)+p(ky,'#FFFFFFFF')))
open('ic_launcher_background.xml','w').write(VEC.format(what='App icon background: midnight.',extra='\n    xmlns:aapt="http://schemas.android.com/aapt"',w=108,body=
'''    <path android:pathData="M0,0h108v108h-108z">
        <aapt:attr name="android:fillColor">
            <gradient
                android:type="linear"
                android:startX="0" android:startY="0"
                android:endX="108" android:endY="108">
                <item android:offset="0" android:color="#FF0E1220" />
                <item android:offset="1" android:color="#FF262E47" />
            </gradient>
        </aapt:attr>
    </path>
'''))
# Status bar: white on transparent, 24dp, filling the box.
sf, sk = door(0.34, 12, 12)
open('ic_stat_sanctum.xml','w').write(VEC.replace('108','24').format(what='Status bar icon: the door, white on transparent.',extra='',w=24,body=p(sf,'#FFFFFFFF',True)+p(sk,'#FFFFFFFF')))
