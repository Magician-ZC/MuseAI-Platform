// 头像图片预处理（上传前）：读图 → canvas 压缩到最长边 ≤maxEdge → 导出 base64（不含 data: 前缀）。
// 抽成独立小函数便于测试（jsdom 无真实 canvas/Image 解码，测试里对本模块整体 mock，不依赖真实渲染）。

/** 压缩结果：纯 base64（不含 `data:...;base64,` 前缀）+ 上传用 MIME。 */
export interface AvatarImageData {
  imageBase64: string;
  mime: 'image/png' | 'image/jpeg';
}

/** 上传前允许的原图 MIME。 */
export const ACCEPTED_AVATAR_MIME = ['image/png', 'image/jpeg', 'image/webp'] as const;

function readAsDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error ?? new Error('读取图片失败'));
    reader.readAsDataURL(file);
  });
}

function loadImage(dataUrl: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error('图片解码失败'));
    img.src = dataUrl;
  });
}

/** 从 data URL 剥出纯 base64 与 MIME。 */
function stripDataUrl(dataUrl: string): AvatarImageData {
  const m = /^data:([^;]+);base64,(.*)$/i.exec(dataUrl);
  const rawMime = m?.[1] ?? 'image/png';
  const mime: AvatarImageData['mime'] = rawMime === 'image/jpeg' ? 'image/jpeg' : 'image/png';
  return { imageBase64: m?.[2] ?? '', mime };
}

/**
 * 压缩头像：等比缩放到最长边 ≤maxEdge（默认 256），jpeg 源保留 jpeg、其余（png/webp）导出 png。
 * 无 canvas 环境（如未 polyfill 的 jsdom）优雅回退为原图 base64，绝不抛异常打断上传流程。
 */
export async function compressAvatarImage(file: Blob, maxEdge = 256): Promise<AvatarImageData> {
  const dataUrl = await readAsDataUrl(file);
  const mime: AvatarImageData['mime'] = file.type === 'image/jpeg' ? 'image/jpeg' : 'image/png';
  try {
    const img = await loadImage(dataUrl);
    const w0 = img.naturalWidth || img.width;
    const h0 = img.naturalHeight || img.height;
    const longest = Math.max(w0, h0);
    const scale = longest > 0 ? Math.min(1, maxEdge / longest) : 1;
    const w = Math.max(1, Math.round(w0 * scale));
    const h = Math.max(1, Math.round(h0 * scale));

    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext('2d');
    if (!ctx || typeof canvas.toDataURL !== 'function') return stripDataUrl(dataUrl);
    ctx.drawImage(img, 0, 0, w, h);
    const out = canvas.toDataURL(mime, 0.9);
    // 极端环境下 toDataURL 可能返回空串 / 'data:,'：回退原图。
    if (!out || !out.includes(';base64,')) return stripDataUrl(dataUrl);
    return stripDataUrl(out);
  } catch {
    return stripDataUrl(dataUrl);
  }
}
