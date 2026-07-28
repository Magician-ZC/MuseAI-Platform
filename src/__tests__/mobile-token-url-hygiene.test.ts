import { beforeEach, describe, expect, it } from 'vitest';
import { clearMobileToken, getMobileToken, stripMobileTokenFromUrl } from '../utils/runtime';
import mobileHomeSrc from '../pages/MobileHome.tsx?raw';

/**
 * 🔴 手机端是扫码打开 `http://<内网 IP>:<端口>/?token=xxx` 进来的，**令牌就写在 URL 里**。
 *
 * 服务端在首次加载 `/` 时把它落成 `HttpOnly; SameSite=Lax` cookie，此后请求靠 cookie 即可
 * （`credentials: 'same-origin'`），URL 里那份已经不需要。
 *
 * 而在 2026-07-28 之前，`clearMobileToken()` **只在验证失败的 catch 分支被调用**——
 * 也就是说**只有令牌无效时才抹**，成功路径上它整个会话都留在地址栏：
 * 浏览器历史、书签、截图投屏、以及「把这个链接发给另一台设备」全都会把令牌一起带走。
 * 而拿到令牌的人可以调用手机端全部接口。
 */
describe('手机端令牌不得滞留在地址栏', () => {
  const setUrl = (search: string) => {
    window.history.replaceState(null, '', `/${search}`);
  };

  beforeEach(() => {
    setUrl('');
    clearMobileToken();
  });

  it('stripMobileTokenFromUrl 抹掉 token 但保留其它查询参数', () => {
    setUrl('?token=secret-abc&tab=chat');
    stripMobileTokenFromUrl();
    expect(window.location.search).not.toContain('secret-abc');
    expect(window.location.search).toContain('tab=chat');
  });

  it('🔴 只抹 URL，**不清内存里的令牌**', () => {
    setUrl('?token=secret-abc');
    expect(getMobileToken()).toBe('secret-abc'); // 先读进内存
    stripMobileTokenFromUrl();
    expect(window.location.search).not.toContain('secret-abc');
    expect(getMobileToken()).toBe(
      'secret-abc',
    ); // 内存里仍在：cookie 尚未生效的那一瞬还要靠它
  });

  it('clearMobileToken 是登出语义：内存与 URL 都清', () => {
    setUrl('?token=secret-abc');
    expect(getMobileToken()).toBe('secret-abc');
    clearMobileToken();
    expect(window.location.search).not.toContain('secret-abc');
    expect(getMobileToken()).toBe('');
  });

  it('没有 token 参数时不动 URL（不产生多余的 history 记录）', () => {
    setUrl('?tab=chat');
    const before = window.location.search;
    stripMobileTokenFromUrl();
    expect(window.location.search).toBe(before);
  });
});

/**
 * 🔴 **接线红线**：光有 `stripMobileTokenFromUrl` 没用，它必须真的挂在**验证成功**那条路上。
 *
 * 改动前的状态恰恰是「函数存在、但只在失败分支被调用」——所以这一条钉的不是函数存在，
 * 而是它出现在 `setConnectionStatus('verified')` 之后、且不是只出现在 catch 里。
 */
describe('清理必须挂在验证成功路径上', () => {
  it('MobileHome 在验证成功后调用 stripMobileTokenFromUrl', () => {
    const verified = mobileHomeSrc.indexOf("setConnectionStatus('verified')");
    expect(verified, "找不到验证成功的落点 —— 本断言的前提变了，请重新确认接线").toBeGreaterThan(-1);

    const strip = mobileHomeSrc.indexOf('stripMobileTokenFromUrl()', verified);
    expect(
      strip,
      '🔴 验证成功后没有抹掉地址栏里的 token —— 它会随历史/书签/截图/转发一起泄露。' +
        '（改动前正是这个状态：清理函数只在 catch 分支被调用，成功路径一直留着令牌。）',
    ).toBeGreaterThan(-1);

    const catchIdx = mobileHomeSrc.indexOf('} catch');
    expect(
      strip < catchIdx || catchIdx === -1,
      '🔴 清理调用落在 catch 之后 —— 那又回到了「只有失败才抹」',
    ).toBe(true);
  });
});
