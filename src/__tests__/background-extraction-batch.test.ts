import { describe, expect, it, vi } from 'vitest';
import { parseBackgroundExtractionError, runCharacterExtractionBatch, splitCharacterNames } from '../utils/backgroundExtraction';

describe('background character extraction helpers', () => {
  it('splits manual character names and removes duplicates', () => {
    expect(splitCharacterNames('林逸\n陆雪莹，林逸、唐小山;  ')).toEqual(['林逸', '陆雪莹', '唐小山']);
  });

  it('runs character-card extraction with a concurrency limit of 5', async () => {
    let active = 0;
    let maxActive = 0;
    const resolvers: Array<() => void> = [];

    const promise = runCharacterExtractionBatch({
      names: ['A', 'B', 'C', 'D', 'E', 'F'],
      worker: async (name) => {
        active += 1;
        maxActive = Math.max(maxActive, active);
        await new Promise<void>((resolve) => resolvers.push(resolve));
        active -= 1;
        return { name };
      },
    });

    await Promise.resolve();
    expect(maxActive).toBe(5);
    expect(resolvers).toHaveLength(5);

    resolvers.splice(0).forEach((resolve) => resolve());
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(maxActive).toBe(5);
    expect(resolvers).toHaveLength(1);

    resolvers.splice(0).forEach((resolve) => resolve());
    const results = await promise;

    expect(results).toHaveLength(6);
    expect(results.every((item) => item.status === 'success')).toBe(true);
  });

  it('waits for active workers to finish after abort before resolving', async () => {
    vi.useFakeTimers();
    try {
      const controller = new AbortController();
      const resolvers: Array<() => void> = [];
      let resolved = false;

      const promise = runCharacterExtractionBatch({
        names: ['A'],
        signal: controller.signal,
        worker: async (name) => {
          await new Promise<void>((resolve) => resolvers.push(resolve));
          return { name };
        },
      }).then((result) => {
        resolved = true;
        return result;
      });

      await Promise.resolve();
      expect(resolvers).toHaveLength(1);

      controller.abort();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(resolved).toBe(false);

      resolvers[0]();
      const results = await promise;

      expect(resolved).toBe(true);
      expect(results).toEqual([{ name: 'A', status: 'pending' }]);
    } finally {
      vi.useRealTimers();
    }
  });

  it('keeps backend string errors unprocessed for failed character details', () => {
    const rawError = '模型没有返回合法 JSON，请重新分析：模型返回的 JSON 被截断了（输出超长）。\n建议：1）减少选中文件数量。\n\n---RAW_MODEL_OUTPUT_START---\n{"name":"邓布利多","fields":{"backgroundStory":"被截断"';
    const parsed = parseBackgroundExtractionError(rawError);

    expect(parsed.message).toBe(rawError);
    expect(parsed.rawOutput).toBe(rawError);
  });

  it('keeps backend object errors visible as the original received payload', () => {
    const rawError = {
      error: JSON.stringify({
        message: '模型没有返回合法 JSON，请重新分析：模型返回的 JSON 被截断了（输出超长）。',
        rawOutput: '{"name":"莱姆斯","fields":{"backgroundStory":"原始片段"',
      }),
    };
    const parsed = parseBackgroundExtractionError(rawError);

    expect(parsed.rawOutput).toContain('"error"');
    expect(parsed.rawOutput).toContain('莱姆斯');
  });
});
