from PIL import Image, ImageDraw

# Lucide Activity waveform, simplified from lucide-activity.svg for raster output.
scale = 4
im = Image.new('RGBA', (256 * scale, 256 * scale))
d = ImageDraw.Draw(im)
def xy(p):
    return tuple(round(v * scale) for v in p)
d.rounded_rectangle((0, 0, 256 * scale - 1, 256 * scale - 1), radius=57 * scale, fill='#e3e9fb')
# Original 24x24 icon is centered with ample space at small sizes.
pts = [(22,12),(19.52,12),(18.65,12.2),(17.95,12.75),(17.59,13.46),(15.24,21.82),(15,22),(14.76,21.82),(9.24,2.18),(9,2),(8.76,2.18),(6.41,10.54),(6.05,11.25),(5.36,11.8),(4.49,12),(2,12)]
path = [xy((48 + x * (160 / 24), 48 + y * (160 / 24))) for x, y in pts]
width = round(15 * scale)
d.line(path, fill='#425b9e', width=width, joint='curve')
for x, y in (path[0], path[-1]):
    d.ellipse((x-width//2, y-width//2, x+width//2, y+width//2), fill='#425b9e')
im = im.resize((256, 256), Image.Resampling.LANCZOS)
im.save('assets/pulse.ico', format='ICO', sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])
