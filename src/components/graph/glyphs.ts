// 图节点字形：把角色/地点渲染成 SVG data URI，供 echarts `symbol: 'image://...'` 使用。
// - 角色：MuseAI 角色为用户原创、无立绘，故用「名字首字 + 弧光配色环」的圆形头像，
//   近似 novel-fan-graph 的圆形头像布局（真立绘需另配 AI 生图/上传头像来源）。
// - 地点：无类型字段，但可从 name 关键词映射语义 emoji（圆角方块背景 + emoji 居中）。
// 借鉴通用可视化范式（图标化节点），不复用任何 novel-fan-graph 代码或资源。

/** UTF-8 安全的 base64（SVG 含中文/emoji）。 */
function b64(s: string): string {
  return btoa(unescape(encodeURIComponent(s)));
}
function escapeXml(s: string): string {
  return s.replace(/[<>&'"]/g, (c) => ({ '<': '&lt;', '>': '&gt;', '&': '&amp;', "'": '&apos;', '"': '&quot;' }[c] as string));
}
/** 把一个 hex 颜色提亮（径向高光用），比例 0~1。 */
function lighten(hex: string, amount = 0.28): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return hex;
  const n = parseInt(m[1], 16);
  const r = (n >> 16) & 255, g = (n >> 8) & 255, b = n & 255;
  const mix = (c: number) => Math.round(c + (255 - c) * amount);
  return `#${((mix(r) << 16) | (mix(g) << 8) | mix(b)).toString(16).padStart(6, '0')}`;
}

/** 角色首字圆牌：径向高光填充 + 白色首字 + 弧光/我方配色环。 */
export function charAvatarDataUri(opts: {
  name: string; fill: string; ring: string; ringWidth?: number; size?: number;
}): string {
  const size = opts.size ?? 100;
  const rw = opts.ringWidth ?? 4;
  const ch = escapeXml(((opts.name || '?').trim()[0]) || '?');
  const r = size / 2;
  const inner = r - rw - 1;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">` +
    `<defs><radialGradient id="g" cx="38%" cy="30%" r="80%">` +
    `<stop offset="0%" stop-color="${lighten(opts.fill)}"/><stop offset="100%" stop-color="${opts.fill}"/>` +
    `</radialGradient></defs>` +
    `<circle cx="${r}" cy="${r}" r="${inner}" fill="url(#g)"/>` +
    `<circle cx="${r}" cy="${r}" r="${inner}" fill="none" stroke="${opts.ring}" stroke-width="${rw}"/>` +
    `<text x="${r}" y="${r + size * 0.02}" text-anchor="middle" dominant-baseline="central" ` +
    `font-family="'PingFang SC','Hiragino Sans GB','Noto Sans SC',sans-serif" font-size="${(size * 0.44).toFixed(1)}" ` +
    `font-weight="600" fill="#fffdfa">${ch}</text>` +
    `</svg>`;
  return 'image://data:image/svg+xml;base64,' + b64(svg);
}

/** 地点名 → 语义 emoji（无类型字段时的启发式映射；秘境优先）。 */
const LOC_ICON_RULES: Array<[RegExp, string]> = [
  [/秘境|幻境|镜花|禁地/, '🔮'],
  [/拍卖/, '🔨'],
  [/坊市|集市|市集|商铺|铺/, '🏪'],
  [/学院|书院|学堂|广场/, '📚'],
  [/斗技|演武|比武|擂台/, '⚔️'],
  [/训练|练功/, '🎯'],
  [/后山|山峰|山脉|山/, '⛰️'],
  [/斗气阁|藏经|阁|楼/, '🗼'],
  [/迎客|大厅|正厅|厅|堂/, '🏮'],
  [/府邸|宅邸|宅|府/, '🏯'],
  [/大道|官道|街道|街|道/, '🛣️'],
  [/大陆|世界|界/, '🗺️'],
  [/城/, '🏙️'],
];
export function locationEmoji(name: string, secret: boolean): string {
  if (secret) return '🔮';
  for (const [re, ic] of LOC_ICON_RULES) if (re.test(name || '')) return ic;
  return '📍';
}

/** 地点图标：圆角方块背景 + emoji 居中（秘境用紫色虚线框）。 */
export function locationIconDataUri(opts: {
  name: string; secret: boolean; size?: number;
}): string {
  const size = opts.size ?? 68;
  const emoji = locationEmoji(opts.name, opts.secret);
  const pad = 4;
  const inner = size - pad * 2;
  const rad = inner * 0.26;
  const bg = opts.secret ? '#efe2f3' : '#f3ece0';
  const border = opts.secret ? '#a06db0' : '#b8a892';
  const dash = opts.secret ? ` stroke-dasharray="4 3"` : '';
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">` +
    `<rect x="${pad}" y="${pad}" width="${inner}" height="${inner}" rx="${rad}" ry="${rad}" ` +
    `fill="${bg}" stroke="${border}" stroke-width="1.5"${dash}/>` +
    `<text x="${size / 2}" y="${size / 2 + size * 0.04}" text-anchor="middle" dominant-baseline="central" ` +
    `font-size="${(inner * 0.5).toFixed(1)}">${emoji}</text>` +
    `</svg>`;
  return 'image://data:image/svg+xml;base64,' + b64(svg);
}
