import type { ThemeConfig } from 'antd';

/** 客户端中文字体栈（设计文档 §5）：系统中文无衬线字体，兼容 PingFang SC / Hiragino Sans GB / Microsoft YaHei。 */
export const CLIENT_FONT_FAMILY =
  'Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", "Helvetica Neue", Arial, sans-serif';

export const warmMinimalistTheme: ThemeConfig = {
  token: {
    // Warm background and text colors（设计文档 §5：页面底色 #fbfaf7 暖白）
    colorBgBase: '#fbfaf7', // Warm off-white
    colorTextBase: '#33312e', // Deep warm gray
    colorPrimary: '#d97757', // Soft terracotta accent

    // UI elements styling
    colorBgContainer: '#ffffff', // Clean white for surface
    colorBorder: '#eae6df', // Very subtle border
    borderRadius: 8,

    // Typography
    fontFamily: CLIENT_FONT_FAMILY,
  },
  components: {
    Layout: {
      siderBg: '#fbfaf7',
      headerBg: '#fbfaf7',
      bodyBg: '#fbfaf7',
    },
    Menu: {
      itemBg: '#fbfaf7',
      itemSelectedBg: '#f2e8dc',
      itemSelectedColor: '#d97757',
    },
  },
};
