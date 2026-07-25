import React from 'react';
import ReactDOM from 'react-dom/client';
import { App as AntdApp, ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import App from './App';
import './admin.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ConfigProvider
      locale={zhCN}
      theme={{
        // 色值以 docs/design/admin-ui-design.md §5 为准，不引入表外近似值。
        token: {
          colorPrimary: '#c96845',
          colorInfo: '#c96845',
          colorSuccess: '#749b63',
          colorWarning: '#d99535',
          colorError: '#ca5547',
          colorText: '#2e2b28',
          colorTextSecondary: '#827b73',
          colorBorder: '#e4ded6',
          colorBgBase: '#fbfaf7',
          colorBgLayout: '#fbfaf7',
          colorBgContainer: '#fffdfa',
          borderRadius: 8,
          boxShadow: '0 1px 2px rgba(55, 44, 34, 0.04)',
          fontFamily: 'Inter, ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
        },
        components: {
          Button: { controlHeight: 36, primaryShadow: 'none' },
          Table: { headerBg: '#fbfaf7', rowHoverBg: '#faf5ef' },
          Layout: { bodyBg: '#fbfaf7', headerBg: '#fbfaf7', siderBg: '#fbfaf7' },
        },
      }}
    >
      <AntdApp>
        <App />
      </AntdApp>
    </ConfigProvider>
  </React.StrictMode>
);
