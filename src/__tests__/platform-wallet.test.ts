import { describe, expect, it } from 'vitest';
import { useWalletStore } from '../stores/useWalletStore';

describe('useWalletStore — P4a 占位（未启用）', () => {
  it('钱包恒为未启用、零余额', () => {
    const s = useWalletStore.getState();
    expect(s.enabled).toBe(false);
    expect(s.balance).toBe(0);
    expect(s.status).toBe('disabled');
    expect(s.statusLabel()).toBe('未启用');
  });
});
