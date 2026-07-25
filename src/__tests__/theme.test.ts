import { describe, it, expect } from 'vitest';
import { warmMinimalistTheme } from '../theme';

describe('warmMinimalistTheme', () => {
  it('should export a valid theme config object', () => {
    expect(warmMinimalistTheme).toBeDefined();
    expect(warmMinimalistTheme.token).toBeDefined();
    expect(warmMinimalistTheme.components).toBeDefined();
  });

  // 设计文档 docs/design/client-ui-design.md §5：页面底色 #fbfaf7 暖白，卡片底色 #ffffff。
  it('should have warm background colors', () => {
    expect(warmMinimalistTheme.token?.colorBgBase).toBe('#fbfaf7');
    expect(warmMinimalistTheme.token?.colorBgContainer).toBe('#ffffff');
  });

  it('should have terracotta accent color as primary', () => {
    expect(warmMinimalistTheme.token?.colorPrimary).toBe('#d97757');
  });

  it('should have deep warm gray text color', () => {
    expect(warmMinimalistTheme.token?.colorTextBase).toBe('#33312e');
  });

  it('should have subtle border color', () => {
    expect(warmMinimalistTheme.token?.colorBorder).toBe('#eae6df');
  });

  it('should have 8px border radius', () => {
    expect(warmMinimalistTheme.token?.borderRadius).toBe(8);
  });

  it('should configure Layout component colors', () => {
    const layout = warmMinimalistTheme.components?.Layout;
    expect(layout).toBeDefined();
    expect(layout?.siderBg).toBe('#fbfaf7');
    expect(layout?.headerBg).toBe('#fbfaf7');
    expect(layout?.bodyBg).toBe('#fbfaf7');
  });

  it('should configure Menu component colors', () => {
    const menu = warmMinimalistTheme.components?.Menu;
    expect(menu).toBeDefined();
    expect(menu?.itemBg).toBe('#fbfaf7');
    expect(menu?.itemSelectedBg).toBe('#f2e8dc');
    expect(menu?.itemSelectedColor).toBe('#d97757');
  });

  // 设计文档 §5：中文字体栈必须兼容 PingFang SC / Hiragino Sans GB / Microsoft YaHei。
  it('should declare a CJK-capable font stack', () => {
    const fontFamily = warmMinimalistTheme.token?.fontFamily ?? '';
    expect(fontFamily).toContain('PingFang SC');
    expect(fontFamily).toContain('Hiragino Sans GB');
    expect(fontFamily).toContain('Microsoft YaHei');
  });
});
