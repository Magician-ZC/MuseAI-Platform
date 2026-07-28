import { CloudError, cloudFetch } from '../../../utils/cloudApi';

export interface JourneyMembership {
  worldId: string;
  worldTitle: string;
  worldStatus: string;
  roomType: string;
  cloudCharacterId: string;
  characterName: string;
  membershipStatus: string;
}

export interface JourneyContext {
  worldId: string;
  worldTitle: string;
  characterId: string;
  characterName: string;
}

export interface SubplotCard {
  id: string;
  starRating: number;
  label: string;
  originKind: string;
  status: string;
  source?: { worldId?: string | null; templateId?: string | null };
  synthesizedFrom?: string[];
}

export interface Invitation {
  id: string;
  worldId: string;
  worldTitle: string;
  inviterCharacterName: string;
  myCharacterId: string;
  myCharacterName: string;
  expiresAt: number;
  createdAt: number;
}

export const previewContext: JourneyContext = {
  worldId: 'world-mist-sea',
  worldTitle: '雾海纪元',
  characterId: 'char-kane',
  characterName: '凯恩·夜誓',
};

export const previewMemberships: JourneyMembership[] = [
  {
    ...previewContext,
    cloudCharacterId: previewContext.characterId,
    worldStatus: 'running',
    roomType: 'chapter',
    membershipStatus: 'active',
  },
];

export const previewCards: SubplotCard[] = [
  { id: 'subplot-01', starRating: 2, label: '未寄出的潮汐信', originKind: 'settlement', status: 'owned', source: { worldId: previewContext.worldId } },
  { id: 'subplot-02', starRating: 2, label: '灯塔守望者的旧约', originKind: 'settlement', status: 'owned', source: { worldId: previewContext.worldId } },
  { id: 'subplot-03', starRating: 2, label: '沉船中的第三个名字', originKind: 'settlement', status: 'owned', source: { worldId: previewContext.worldId } },
  { id: 'subplot-04', starRating: 1, label: '酒馆窗边的耳语', originKind: 'settlement', status: 'owned', source: { worldId: 'world-ember-tavern' } },
  { id: 'subplot-05', starRating: 1, label: '雨夜里的备用钥匙', originKind: 'settlement', status: 'owned', source: { worldId: 'world-ember-tavern' } },
  { id: 'subplot-06', starRating: 3, label: '越过雾墙的人', originKind: 'synthesis', status: 'owned', source: { worldId: previewContext.worldId } },
];

export const previewInvitations: Invitation[] = [
  {
    id: 'invite-01',
    worldId: 'world-ember-tavern',
    worldTitle: '余烬酒馆',
    inviterCharacterName: '艾琳娜·风语',
    myCharacterId: previewContext.characterId,
    myCharacterName: previewContext.characterName,
    createdAt: Date.now() - 18 * 60 * 1000,
    expiresAt: Date.now() + 22 * 60 * 60 * 1000,
  },
  {
    id: 'invite-02',
    worldId: previewContext.worldId,
    worldTitle: previewContext.worldTitle,
    inviterCharacterName: '索伦·灰塔',
    myCharacterId: previewContext.characterId,
    myCharacterName: previewContext.characterName,
    createdAt: Date.now() - 3 * 60 * 60 * 1000,
    expiresAt: Date.now() + 12 * 60 * 60 * 1000,
  },
];

export function journeyError(error: unknown): string {
  if (error instanceof CloudError) {
    if (error.status === 404) return '该能力尚未在当前环境开放，或对应功能开关尚未启用。';
    return error.message;
  }
  return error instanceof Error ? error.message : '连接平台失败，请稍后再试。';
}

export async function loadJourneyContext(worldId?: string): Promise<{ context: JourneyContext | null; memberships: JourneyMembership[] }> {
  const data = await cloudFetch<{ memberships: JourneyMembership[] }>('/api/me/memberships');
  const memberships = data.memberships ?? [];
  const selected = memberships.find((item) => item.worldId === worldId) ?? memberships.find((item) => item.membershipStatus === 'active') ?? memberships[0];
  return {
    memberships,
    context: selected
      ? {
          worldId: selected.worldId,
          worldTitle: selected.worldTitle || selected.worldId,
          characterId: selected.cloudCharacterId,
          characterName: selected.characterName || selected.cloudCharacterId,
        }
      : null,
  };
}

export function formatJourneyTime(value?: number | null): string {
  if (!value) return '—';
  return new Intl.DateTimeFormat('zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  }).format(new Date(value));
}
