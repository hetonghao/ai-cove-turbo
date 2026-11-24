# Turbo 图片压缩设计：离线样本（2026-11-24）

这是设计样机，不经过真实 Turbo/New API、上游或公网。固定种子生成带文字、队列和小块热图的 RGB 截图，不读取用户图片、会话或凭据；只做无损转换。比较对照和处理组时均使用 zstd 级别 3。所有数字为一次本地运行，热请求取同一批 5 次的中位数。

| 场景 | 缓存命中 | 编码次数 | 图片二进制 | JSON | zstd 后 HTTP 应用载荷 | 本地处理到打包 |
|---|---:|---:|---:|---:|---:|---:|
| 原 PNG + zstd | — | 0 | 5,339,027 B | 7,120,784 B | 5,197,129 B | 未计时 |
| 无损 + zstd，32 张新图 | 0 | 32 | 4,385,184 B | 5,849,032 B | 4,393,908 B | 8,837.5 ms |
| 重发相同 32 张 | 32 | 0 | 4,385,184 B | 5,849,032 B | 4,393,908 B | 80.7 ms |
| 31 张旧图 + 1 张新图 | 31 | 1 | 4,385,704 B | 5,849,724 B | 4,394,933 B | 390.8 ms |

原图每张 166,533–167,045 B，全部低于 512,000 B。以整批触发后，两组 zstd 载荷相差 803,221 B（15.46%）；33 张图转换前后的 RGB 像素逐一相同。计时含解析 JSON、Base64、散列查表、必要时串行编码、重建 JSON 及 zstd；不含造图、最终像素验证、网络与模型。这个内存字典仅模拟一个账号一种固定策略的热/冷路径，不是并发、配额或跨进程验证。编码器版本、图片内容与设备变化都会影响结果。

复现：Python 3，Pillow 10.3.0 / libwebp 1.3.2、NumPy 1.26.4、zstandard 0.22.0；复制下列程序在内存中运行。

```python
"""Offline request-size comparison for the Turbo image design. Synthetic images only.
Run with Python, Pillow (WebP), NumPy and zstandard. Writes no images or requests to disk.
"""
import base64
import io
import hashlib
import statistics
import json
import time

import numpy as np
import PIL
from PIL import Image, ImageDraw, features
import zstandard as zstd


def screenshot(i):
    img = Image.new('RGB', (960, 640), '#f7f8fa')
    draw = ImageDraw.Draw(img)
    draw.rectangle((0, 0, 959, 47), fill='#172b42')
    draw.text((20, 17), f'Queue monitor / capture {i + 1:02d}', fill='white')
    rng = np.random.default_rng(20261124 + i)
    for row in range(22):
        y = 66 + 25 * row
        draw.line((20, y + 17, 940, y + 17), fill='#d9dfe6')
        draw.text((22, y), f'Worker {row + 1:02d}', fill='#263a4d')
        width = int(rng.integers(20, 335))
        draw.rectangle((145, y + 2, 145 + width, y + 10), fill='#287f91')
    # A heatmap-like thumbnail adds continuous colour to a text-heavy screen.
    y, x = np.indices((210, 320))
    noise = rng.integers(0, 24, (210, 320, 3))
    colour = (np.stack((x // 2, y // 2 + 40, (x + y) // 5 + 80), axis=-1) + noise).astype('uint8')
    img.paste(Image.fromarray(colour), (595, 83))
    return img


def encoded(img, fmt, **kw):
    buf = io.BytesIO()
    img.save(buf, format=fmt, **kw)
    return buf.getvalue()


def build(mimes, images):
    content = [{'type': 'input_text', 'text': 'Read the labelled charts.'}]
    for mime, image in zip(mimes, images):
        b64 = base64.b64encode(image).decode('ascii')
        content.append({'type': 'input_image', 'image_url': 'data:image/' + mime + ';base64,' + b64})
    return json.dumps({'model':'fixture', 'input':[{'role':'user','content':content}], 'stream':True}, separators=(',', ':')).encode()


def main():
    def png(i):
        return encoded(screenshot(i), 'PNG', compress_level=6)
    originals = [png(i) for i in range(33)]
    raw = build(['png'] * 32, originals[:32]); gain_one = build(['png'] * 32, originals[1:])
    z = zstd.ZstdCompressor(level=3, write_content_size=False)
    cache = {}
    def request(body):
        start = time.perf_counter()
        hits = encodes = smaller = before = after = 0
        obj = json.loads(body)
        for part in obj['input'][0]['content'][1:]:
            src = base64.b64decode(part['image_url'].split(',',1)[1], validate=True)
            before += len(src)
            key = hashlib.sha256(src).digest()  # one synthetic account, one fixed policy
            if key in cache:
                mime, result = cache[key]
                hits += 1
            else:
                encodes += 1
                with Image.open(io.BytesIO(src)) as img:
                    result = encoded(img, 'WEBP', lossless=True, method=4)
                mime, result = ('webp', result) if len(result)<len(src) else ('png', src)
                cache[key] = (mime,result)
            smaller += int(mime=='webp'); after += len(result)
            part['image_url'] = 'data:image/'+mime+';base64,'+base64.b64encode(result).decode('ascii')
        data = json.dumps(obj, separators=(',', ':')).encode()
        wire = z.compress(data)
        return hits,encodes,smaller,before,after,len(data),len(wire),round(1000*(time.perf_counter()-start),1)
    print('Pillow',PIL.__version__,'WebP',features.version('webp'),'zstandard',zstd.__version__,'NumPy',np.__version__)
    print('PNG bytes min/max, >=500KiB:', min(map(len, originals[:32])),max(map(len, originals[:32])),sum(len(i)>=512000 for i in originals[:32]))
    print('baseline JSON/ZSTD payload bytes:',len(raw), len(z.compress(raw)))
    print('hits, encodes, smaller, image bytes in/out, JSON bytes, zstd payload bytes, elapsed ms')
    print('cold:', request(raw))
    hot = [request(raw) for _ in range(5)]
    print('hot (median of 5):', hot[0][:-1] + (statistics.median(v[-1] for v in hot),))
    print('31 old + 1 new:', request(gain_one))
    for src in originals:
        _, result = cache[hashlib.sha256(src).digest()]
        with Image.open(io.BytesIO(src)) as a, Image.open(io.BytesIO(result)) as b:
            assert a.size == b.size and a.convert('RGB').tobytes() == b.convert('RGB').tobytes()
    print('RGB pixels match for all 33 synthetic images.')


if __name__ == '__main__':
    main()
```
