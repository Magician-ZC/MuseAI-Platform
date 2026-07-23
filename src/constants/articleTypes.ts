export interface ArticleTypeOption {
  value: string;
  label: string;
  children?: ArticleTypeOption[];
}

export const ARTICLE_TYPE_OPTIONS: ArticleTypeOption[] = [
  {
    value: '男频',
    label: '男频',
    children: [
      {
        value: '长篇',
        label: '长篇',
        children: [
          { value: '玄幻脑洞', label: '玄幻脑洞' },
          { value: '西方玄幻', label: '西方玄幻' },
          { value: '东方仙侠', label: '东方仙侠' },
          { value: '历史古代', label: '历史古代' },
          { value: '游戏体育', label: '游戏体育' },
        ],
      },
    ],
  },
  {
    value: '女频',
    label: '女频',
    children: [
      {
        value: '短篇',
        label: '短篇',
        children: [
          { value: '追妻火葬场', label: '追妻火葬场' },
          { value: '大女主', label: '大女主' },
          { value: '系统穿越', label: '系统穿越' },
          { value: '真假千金', label: '真假千金' },
          { value: '规则怪谈', label: '规则怪谈' },
        ],
      },
    ],
  },
  { value: '公众号', label: '公众号' },
];

const WRITER_SKILLS: Record<string, string> = {
  '男频-长篇-玄幻脑洞': 'fanqie-xuanhuan-writer',
  '男频-长篇-西方玄幻': 'fanqie-long-xifang-writer',
  '男频-长篇-东方仙侠': 'fanqie-long-xianxia-writer',
  '男频-长篇-历史古代': 'fanqie-long-lishi-writer',
  '男频-长篇-游戏体育': 'fanqie-long-youxi-writer',
  '女频-短篇-追妻火葬场': 'fanqie-short-zhuiqi-writer',
  '女频-短篇-大女主': 'fanqie-short-danvzhu-writer',
  '女频-短篇-系统穿越': 'fanqie-short-xitong-writer',
  '女频-短篇-真假千金': 'fanqie-short-qianjin-writer',
  '女频-短篇-规则怪谈': 'fanqie-short-guize-writer',
  '公众号': 'kitt-writer',
};

const OUTLINE_SKILLS: Record<string, string> = {
  '男频-长篇-玄幻脑洞': 'fanqie-xuanhuan-outline',
  '男频-长篇-西方玄幻': 'fanqie-long-xifang-outline',
  '男频-长篇-东方仙侠': 'fanqie-long-xianxia-outline',
  '男频-长篇-历史古代': 'fanqie-long-lishi-outline',
  '男频-长篇-游戏体育': 'fanqie-long-youxi-outline',
  '女频-短篇-追妻火葬场': 'fanqie-short-zhuiqi-outline',
  '女频-短篇-大女主': 'fanqie-short-danvzhu-outline',
  '女频-短篇-系统穿越': 'fanqie-short-xitong-outline',
  '女频-短篇-真假千金': 'fanqie-short-qianjin-outline',
  '女频-短篇-规则怪谈': 'fanqie-short-guize-outline',
};

const articleTypeKey = (articleType: readonly string[]) => articleType.join('-');

export const getWriterSkillName = (articleType: readonly string[]) =>
  WRITER_SKILLS[articleTypeKey(articleType)] ?? null;

export const getOutlineSkillName = (articleType: readonly string[]) =>
  OUTLINE_SKILLS[articleTypeKey(articleType)] ?? null;

export const buildRequiredSkillInstruction = (content: string, skillNames: readonly string[]) => {
  if (skillNames.length === 0) return content;
  const slashCommands = skillNames.map((name) => `/${name}`).join('\n');
  return `${slashCommands}\n【系统指令】本轮必须优先调用以下技能：${skillNames.join(', ')}\n\n${content}`;
};
