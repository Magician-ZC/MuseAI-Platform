import { appInvoke } from './runtime';
import { BookTravelMaterial } from '../stores/useBookTravelStore';
import { usePartnerStore, compileItemToMarkdown } from '../stores/usePartnerStore';

const fileNameFromPath = (path: string) => {
  const normalized = path.replace(/\\/g, '/');
  return normalized.split('/').filter(Boolean).pop() || path;
};

export const resolveOutlineMaterial = async (path: string): Promise<BookTravelMaterial> => {
  const content = await appInvoke('read_file', { path });
  return {
    id: path,
    title: fileNameFromPath(path),
    path,
    content,
  };
};

export const resolvePartnerMaterials = (worldBookId: string, characterCardIds: string[]) => {
  const { worldBooks, characterCards } = usePartnerStore.getState();
  const worldBook = worldBooks.find((item) => item.id === worldBookId);
  const selectedCards = characterCardIds
    .map((id) => characterCards.find((item) => item.id === id))
    .filter((item): item is NonNullable<typeof item> => Boolean(item));

  return {
    worldBook: worldBook
      ? { id: worldBook.id, title: worldBook.name, content: worldBook.content }
      : null,
    characterCards: selectedCards.map((card) => {
      let content = card.content;
      if (card.fields) {
        const cleanFields = { ...card.fields };
        delete cleanFields.relationMemory;
        delete cleanFields.userRelationType;
        delete cleanFields.userInteractionModel;
        delete cleanFields.userRelationBottomLine;
        delete cleanFields.keyEvents;

        content = compileItemToMarkdown(card.name, 'character_card', cleanFields);
      }
      return {
        id: card.id,
        title: card.name,
        content,
      };
    }),
  };
};
