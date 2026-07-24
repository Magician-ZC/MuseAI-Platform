// 头像图片预处理（上传前）：读图 → 中心裁方 → 圆形裁剪（角落透明）→ 压缩到边长 ≤maxEdge → 导出 base64。
// 圆形裁剪让真头像与图谱首字圆牌视觉统一；透明角落要求 canvas 路径恒导出 PNG。
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
 * 压缩头像：中心裁方 → 圆形裁剪（角落透明，与图谱首字圆牌视觉统一）→ 缩放到边长 ≤maxEdge（默认 256）。
 * 透明角落要求 canvas 路径恒导出 PNG（jpeg 无 alpha 通道）。
 * 无 canvas 环境（如未 polyfill 的 jsdom）优雅回退为原图 base64，绝不抛异常打断上传流程。
 */
export async function compressAvatarImage(file: Blob, maxEdge = 256): Promise<AvatarImageData> {
  const dataUrl = await readAsDataUrl(file);
  try {
    const img = await loadImage(dataUrl);
    const w0 = img.naturalWidth || img.width;
    const h0 = img.naturalHeight || img.height;
    const side0 = Math.min(w0, h0); // 中心裁方的源边长
    if (side0 <= 0) return stripDataUrl(dataUrl);
    const side = Math.min(maxEdge, side0);
    const sx = (w0 - side0) / 2;
    const sy = (h0 - side0) / 2;

    const canvas = document.createElement('canvas');
    canvas.width = side;
    canvas.height = side;
    const ctx = canvas.getContext('2d');
    if (!ctx || typeof canvas.toDataURL !== 'function') return stripDataUrl(dataUrl);
    // 圆形裁剪：clip 后再绘制，角落保持透明。
    ctx.beginPath();
    ctx.arc(side / 2, side / 2, side / 2, 0, Math.PI * 2);
    ctx.closePath();
    ctx.clip();
    ctx.drawImage(img, sx, sy, side0, side0, 0, 0, side, side);
    const out = canvas.toDataURL('image/png');
    // 极端环境下 toDataURL 可能返回空串 / 'data:,'：回退原图。
    if (!out || !out.includes(';base64,')) return stripDataUrl(dataUrl);
    return stripDataUrl(out);
  } catch {
    return stripDataUrl(dataUrl);
  }
}
