// 占位页（A0）：agent-A1 用真实模块页面替换。
import { Empty } from 'antd';

export default function Placeholder({ module }: { module: string }) {
  return <Empty description={`${module}（待 agent-A1 实现）`} style={{ marginTop: 120 }} />;
}
