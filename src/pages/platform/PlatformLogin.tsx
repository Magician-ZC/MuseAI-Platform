// 平台登录（C1）：手机号验证码登录，接 useAuthStore + cloudFetch /auth。
// Local-first：登录只解锁平台增量能力；页面显式说明本地能力无需登录。
import React, { useEffect, useRef, useState } from 'react';
import { Card, Form, Input, Button, Typography, Alert, Space, Collapse } from 'antd';
import { MobileOutlined, SafetyOutlined, GlobalOutlined } from '@ant-design/icons';
import { useNavigate, useLocation, Navigate } from 'react-router-dom';
import { useAuthStore, type PlatformUser } from '../../stores/useAuthStore';
import { cloudFetch, getPlatformBase, setPlatformBase } from '../../utils/cloudApi';
import { describeCloudError } from '../../stores/usePlatformStore';

const { Title, Text, Paragraph } = Typography;

interface ChallengeResp {
  challengeId: string;
  expiresAt: number;
  devCode?: string;
}
interface ServerUser {
  id: string;
  phone?: string;
  nickname: string;
  ageDeclared: number;
  status: string;
}
interface LoginResp {
  accessToken: string;
  refreshToken: string;
  user?: ServerUser;
}

const PlatformLogin: React.FC = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const isAuthed = useAuthStore((s) => s.isAuthed());
  const setSession = useAuthStore((s) => s.setSession);

  const [phone, setPhone] = useState('');
  const [code, setCode] = useState('');
  const [countdown, setCountdown] = useState(0);
  const [sending, setSending] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [info, setInfo] = useState<string | null>(null);
  const [baseUrl, setBaseUrl] = useState(getPlatformBase());
  const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => () => {
    if (timerRef.current) clearInterval(timerRef.current);
  }, []);

  const from = (location.state as { from?: string } | null)?.from ?? '/platform';

  // 已登录：用 <Navigate> 声明式跳转（避免在渲染期命令式 navigate 造成重渲染循环）。
  if (isAuthed) {
    return <Navigate to={from} replace />;
  }

  const startCountdown = () => {
    if (timerRef.current) clearInterval(timerRef.current);
    setCountdown(60);
    timerRef.current = setInterval(() => {
      setCountdown((c) => {
        if (c <= 1) {
          if (timerRef.current) clearInterval(timerRef.current);
          return 0;
        }
        return c - 1;
      });
    }, 1000);
  };

  const sendCode = async () => {
    setError(null);
    setInfo(null);
    if (!/^\d{6,20}$/.test(phone.trim())) {
      setError('请输入有效的手机号');
      return;
    }
    setSending(true);
    try {
      const resp = await cloudFetch<ChallengeResp>('/api/auth/challenge', {
        method: 'POST',
        body: { phone: phone.trim() },
        idempotent: true,
      });
      startCountdown();
      setInfo(
        resp.devCode
          ? `验证码已发送（开发模式验证码：${resp.devCode}）`
          : '验证码已发送，请查收短信',
      );
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      setSending(false);
    }
  };

  const login = async () => {
    setError(null);
    if (!phone.trim() || !code.trim()) {
      setError('请输入手机号与验证码');
      return;
    }
    setSubmitting(true);
    try {
      const resp = await cloudFetch<LoginResp>('/api/auth/login', {
        method: 'POST',
        body: { phone: phone.trim(), code: code.trim() },
        idempotent: true,
      });
      const su = resp.user;
      const user: PlatformUser = {
        id: su?.id ?? '',
        nickname: su?.nickname || su?.phone || phone.trim(),
        phone: su?.phone ?? phone.trim(),
        ageDeclared: su?.ageDeclared ?? 0,
      };
      setSession(resp.accessToken, resp.refreshToken, user);
      navigate(from, { replace: true });
    } catch (e) {
      setError(describeCloudError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div style={{ display: 'flex', justifyContent: 'center', padding: '64px 24px' }}>
      <Card
        style={{ width: 420, borderRadius: 14, border: 'none', boxShadow: '0 4px 24px rgba(0,0,0,0.06)' }}
        styles={{ body: { padding: 32 } }}
      >
        <Space direction="vertical" size={4} style={{ width: '100%', marginBottom: 20 }}>
          <GlobalOutlined style={{ fontSize: 28, color: '#d97757' }} />
          <Title level={3} style={{ margin: 0, color: '#33312e' }}>
            登录平台世界
          </Title>
          <Text type="secondary">把你养出的角色投进有别人角色的世界</Text>
        </Space>

        {error && (
          <Alert type="error" showIcon message={error} style={{ marginBottom: 16 }} closable onClose={() => setError(null)} />
        )}
        {info && (
          <Alert type="info" showIcon message={info} style={{ marginBottom: 16 }} closable onClose={() => setInfo(null)} />
        )}

        <Form layout="vertical" onFinish={login}>
          <Form.Item label="手机号">
            <Input
              size="large"
              prefix={<MobileOutlined />}
              placeholder="请输入手机号"
              value={phone}
              onChange={(e) => setPhone(e.target.value)}
              maxLength={20}
              aria-label="手机号"
            />
          </Form.Item>
          <Form.Item label="验证码">
            <Space.Compact style={{ width: '100%' }}>
              <Input
                size="large"
                prefix={<SafetyOutlined />}
                placeholder="6 位验证码"
                value={code}
                onChange={(e) => setCode(e.target.value)}
                maxLength={6}
                aria-label="验证码"
              />
              <Button
                size="large"
                onClick={sendCode}
                loading={sending}
                disabled={countdown > 0}
                style={{ minWidth: 120 }}
              >
                {countdown > 0 ? `${countdown}s 后重发` : '获取验证码'}
              </Button>
            </Space.Compact>
          </Form.Item>
          <Button type="primary" size="large" block htmlType="submit" loading={submitting}>
            登录 / 注册
          </Button>
        </Form>

        <Paragraph type="secondary" style={{ fontSize: 12, marginTop: 20, marginBottom: 0 }}>
          本地模式（角色提取、DNA 卡、单机叙事）无需登录，永不因平台不可用而降级。登录只解锁平台增量能力。
        </Paragraph>

        <Collapse
          ghost
          style={{ marginTop: 8 }}
          items={[
            {
              key: 'adv',
              label: <Text style={{ fontSize: 12 }}>高级：平台服务地址</Text>,
              children: (
                <Space.Compact style={{ width: '100%' }}>
                  <Input
                    value={baseUrl}
                    onChange={(e) => setBaseUrl(e.target.value)}
                    placeholder="http://127.0.0.1:8787"
                    aria-label="平台服务地址"
                  />
                  <Button
                    onClick={() => {
                      setPlatformBase(baseUrl.trim());
                      setInfo('平台地址已保存');
                    }}
                  >
                    保存
                  </Button>
                </Space.Compact>
              ),
            },
          ]}
        />
      </Card>
    </div>
  );
};

export default PlatformLogin;
