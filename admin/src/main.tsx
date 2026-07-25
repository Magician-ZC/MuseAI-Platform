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
        token: {
          colorPrimary: '#c96d49',
          colorInfo: '#c96d49',
          colorSuccess: '#6f985e',
          colorWarning: '#d8943a',
          colorError: '#c95345',
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
