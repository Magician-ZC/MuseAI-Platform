interface TitledCharacter {
  title?: string | null;
}

export const buildBookTravelCharacterDetails = (characters: TitledCharacter[]): string[] => {
  const details = characters.flatMap((character) => (
    character.title ? [`角色卡：${character.title}`] : []
  ));
  return details.length > 0 ? details.slice(0, 3) : ['未绑定角色卡'];
};
