import { describe, expect, it } from 'vitest';
import { buildBookTravelCharacterDetails } from '../utils/bookTravelSaveDetails';

describe('穿书保存进度角色详情', () => {
  it('过滤空标题、保持顺序并最多展示三个角色', () => {
    expect(buildBookTravelCharacterDetails([
      { title: '林晚' },
      { title: '' },
      { title: '沈霜' },
      { title: undefined },
      { title: '顾临' },
      { title: '第四人' },
    ])).toEqual([
      '角色卡：林晚',
      '角色卡：沈霜',
      '角色卡：顾临',
    ]);
  });

  it('没有有效角色标题时返回原有兜底文案', () => {
    expect(buildBookTravelCharacterDetails([
      { title: '' },
      { title: undefined },
    ])).toEqual(['未绑定角色卡']);
  });
});
