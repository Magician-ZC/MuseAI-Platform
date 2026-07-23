import { describe, it, expect } from 'vitest';
import { warmMinimalistTheme } from '../theme';

describe('warmMinimalistTheme', () => {
  it('should export a valid theme config object', () => {
    expect(warmMinimalistTheme).toBeDefined();
    expect(warmMinimalistTheme.token).toBeDefined();
    expect(warmMinimalistTheme.components).toBeDefined();
  });

  it('should have warm background colors', () => {
    expect(warmMinimalistTheme.token?.colorBgBase).toBe('#faf9f5');
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
    expect(layout?.siderBg).toBe('#faf9f5');
    expect(layout?.headerBg).toBe('#faf9f5');
    expect(layout?.bodyBg).toBe('#ffffff');
  });

  it('should configure Menu component colors', () => {
    const menu = warmMinimalistTheme.components?.Menu;
    expect(menu).toBeDefined();
    expect(menu?.itemBg).toBe('#faf9f5');
    expect(menu?.itemSelectedBg).toBe('#f2e8dc');
    expect(menu?.itemSelectedColor).toBe('#d97757');
  });
});
