// 后台登录（A1）：接 /admin/dev-login，用约定 secret 引导换取 admin token。
// 生产由运维将账号提权为 role='admin' 后走正式登录；dev 态用此引导页联调八模块。
import { useState } from 'react';
import { Alert, Button, Card, Form, Input, message } from 'antd';
import { useNavigate } from 'react-router-dom';
import { adminFetch, setRole, setToken } from '../api';
import { friendlyError } from '../components/shared';
import { firstModuleKey } from '../rbac';

const DEFAULT_SECRET = 'muse-dev-admin';

export default function Login() {
  const [loading, setLoading] = useState(false);
  const navigate = useNavigate();

  const onFinish = async (values: { secret: string }) => {
    setLoading(true);
    try {
      const res = await adminFetch<{ accessToken: string; role: string; userId?: string }>(
        '/admin/dev-login',
        'POST',
        { secret: values.secret },
      );
      setToken(res.accessToken);
      // #9 RBAC：保存后端返回的 role，前端据此收敛可见模块。
      setRole(res.role);
      message.success('登录成功');
      // 落到该角色的首个可见模块（避免落在无权模块吃 403）。
      const landing = firstModuleKey(res.role);
      navigate(landing ? `/${landing}` : '/', { replace: true });
    } catch (e) {
      message.error(friendlyError(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div style={{ display: 'grid', placeItems: 'center', minHeight: '100vh' }}>
      <Card title="MuseAI 管理后台" style={{ width: 380 }}>
        <Alert
          type="info"
          showIcon
          style={{ marginBottom: 16 }}
          message="Dev 引导登录"
          description="dev 态用约定 secret 换取管理员令牌。生产环境此入口停用，改由正式管理员登录。"
        />
        <Form onFinish={onFinish} layout="vertical" initialValues={{ secret: DEFAULT_SECRET }}>
          <Form.Item name="secret" label="引导 Secret" rules={[{ required: true, message: '请输入 secret' }]}>
            <Input.Password placeholder="muse-dev-admin" />
          </Form.Item>
          <Button type="primary" htmlType="submit" loading={loading} block>
            登录
          </Button>
        </Form>
      </Card>
    </div>
  );
}
