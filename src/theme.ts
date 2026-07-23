import type { ThemeConfig } from 'antd';

export const warmMinimalistTheme: ThemeConfig = {
  token: {
    // Warm background and text colors
    colorBgBase: '#faf9f5', // Warm off-white
    colorTextBase: '#33312e', // Deep warm gray
    colorPrimary: '#d97757', // Soft terracotta accent
    
    // UI elements styling
    colorBgContainer: '#ffffff', // Clean white for surface
    colorBorder: '#eae6df', // Very subtle border
    borderRadius: 8,
    
    // Typography
    fontFamily: '"Inter", "Roboto", -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
  },
  components: {
    Layout: {
      siderBg: '#faf9f5',
      headerBg: '#faf9f5',
      bodyBg: '#ffffff',
    },
    Menu: {
      itemBg: '#faf9f5',
      itemSelectedBg: '#f2e8dc',
      itemSelectedColor: '#d97757',
    },
  },
};
