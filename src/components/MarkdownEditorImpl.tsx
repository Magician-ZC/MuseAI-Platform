import React, { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { MarkdownEditorView } from './MarkdownEditorView';

interface MarkdownEditorProps {
  filePath: string | null;
  readOnly?: boolean;
}

type SaveStatus = 'saved' | 'saving' | 'error';

interface EditorFileState {
  content: string;
  savedContent: string;
  imagePreviewSrc: string;
  loading: boolean;
  saveStatus: SaveStatus;
  readError: boolean;
  /**
   * 🔴 `content` 是**从哪个文件读出来的**。null = 当前 content 不属于任何文件
   * （正在加载 / 读失败 / 图片 / 空选择），此时**一个字节都不许落盘**。
   *
   * 由来：`text-load-start` 刻意保留上一个文件的 `content`（换文件时编辑区不闪空白），
   * 于是「filePath 已经是新文件、content 还是旧文件的」这个状态是**真实存在**的一帧——
   * 自动保存那条 effect 在那一帧里看到的正是 `pathToSave = 新文件` + `contentToSave = 旧内容`。
   *
   * ⚠️ **诚实说明：这一条今天是纵深防御，不是在补一个测得出来的洞。**
   * 那一帧之后立刻会有一次重渲染（`text-load-start` 把 `loading` 置真）把计时器清掉，
   * 而重渲染在任何现实情形下都远快于 800ms。故故障注入把这一条单独删掉时**用例是绿的**
   * （`loading` / `readError` 各自也拦得住），删掉两条才红。留着它的理由不是「它今天在挡什么」，
   * 而是把「不许写」从**几个否定条件恰好都成立**换成**一句肯定的来源断言**：
   * 内容来自哪个文件，被写进了状态里，而不是靠重渲染比计时器快。
   */
  loadedPath: string | null;
}

type EditorFileAction =
  | { type: 'clear' }
  | { type: 'text-load-start' }
  | { type: 'text-load-success'; content: string; path: string }
  | { type: 'text-load-error'; error: unknown }
  | { type: 'image-load-start' }
  | { type: 'image-load-success'; src: string }
  | { type: 'image-load-error'; error: unknown }
  | { type: 'content-changed'; content: string }
  | { type: 'external-refresh'; content: string }
  | { type: 'save-success'; content: string; isLatest: boolean }
  | { type: 'save-error' };

const initialEditorFileState: EditorFileState = {
  content: '',
  savedContent: '',
  imagePreviewSrc: '',
  loading: false,
  saveStatus: 'saved',
  readError: false,
  loadedPath: null,
};

/**
 * 🔴 **「这份内容此刻该不该写进这个文件」的唯一判据。**
 *
 * 防抖保存与**离开时的补写**都必须走它。两处各写一遍的话，将来只会改动其中一处——
 * 而这两处一个管「正常节奏下的保存」、一个管「最后 800 毫秒」，判据不一致的后果
 * 恰恰是最难复现的那一类（只在切文件的瞬间发作）。
 */
const shouldPersist = (state: EditorFileState, filePath: string | null, readOnly: boolean): boolean => {
  if (readOnly || !filePath || isImageFile(filePath)) return false;
  if (state.loading || state.readError) return false;
  // 🔴 内容必须确实来自这个文件，否则就是在把 A 的正文写进 B。
  if (state.loadedPath !== filePath) return false;
  return state.content !== state.savedContent;
};

const editorFileReducer = (state: EditorFileState, action: EditorFileAction): EditorFileState => {
  switch (action.type) {
    case 'clear':
      return initialEditorFileState;
    case 'text-load-start':
      return {
        ...state,
        imagePreviewSrc: '',
        loading: true,
        readError: false,
        // content 仍是上一个文件的（刻意的，避免闪空白），故它此刻**不属于**任何文件。
        loadedPath: null,
      };
    case 'text-load-success':
      return {
        content: action.content,
        savedContent: action.content,
        imagePreviewSrc: '',
        loading: false,
        saveStatus: 'saved',
        readError: false,
        loadedPath: action.path,
      };
    case 'text-load-error':
      return {
        ...state,
        // 🔴 content 变成了一句错误提示。loadedPath = null 是第二道锁：
        // 万一 readError 那道判断将来被谁改松了，这句提示也绝不会被当成正文写回文件。
        content: `**读取文件失败**: ${action.error}`,
        savedContent: '',
        loading: false,
        saveStatus: 'error',
        readError: true,
        loadedPath: null,
      };
    case 'image-load-start':
      return {
        content: '',
        savedContent: '',
        imagePreviewSrc: '',
        loading: true,
        saveStatus: 'saved',
        readError: false,
        loadedPath: null,
      };
    case 'image-load-success':
      return {
        ...state,
        imagePreviewSrc: action.src,
        loading: false,
      };
    case 'image-load-error':
      return {
        ...state,
        content: `**读取图片失败**: ${action.error}`,
        loading: false,
        readError: true,
      };
    case 'content-changed':
      return {
        ...state,
        content: action.content,
        saveStatus: action.content === state.savedContent ? 'saved' : 'saving',
      };
    case 'external-refresh':
      return {
        ...state,
        content: action.content,
        savedContent: action.content,
        saveStatus: 'saved',
      };
    case 'save-success':
      return {
        ...state,
        savedContent: action.content,
        saveStatus: action.isLatest ? 'saved' : state.saveStatus,
      };
    case 'save-error':
      return {
        ...state,
        saveStatus: 'error',
      };
    default:
      return state;
  }
};

const IMAGE_EXTENSIONS = ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg'];

const isImageFile = (path: string) => {
  const extension = path.split('.').pop()?.toLowerCase();
  return extension ? IMAGE_EXTENSIONS.includes(extension) : false;
};

const getDirectoryName = (path: string) => path.replace(/[\\/][^\\/]*$/, '');

const isExternalImageSrc = (src: string) => /^(?:[a-z]+:)?\/\//i.test(src) || src.startsWith('data:') || src.startsWith('#');

const normalizePath = (path: string) => {
  const absolute = path.startsWith('/');
  const parts = path.split('/').filter(Boolean);
  const stack: string[] = [];
  for (const part of parts) {
    if (part === '.') {
      continue;
    }
    if (part === '..') {
      stack.pop();
      continue;
    }
    stack.push(part);
  }
  return `${absolute ? '/' : ''}${stack.join('/')}`;
};

const resolveImageSrc = async (src: string, markdownPath: string) => {
  if (isExternalImageSrc(src)) {
    return src;
  }
  const absolutePath = src.startsWith('/')
    ? src
    : normalizePath(`${getDirectoryName(markdownPath)}/${src}`);
  return invoke<string>('read_image_data_url', { path: absolutePath });
};

const safeMarkdownUrlTransform = (url: string) => {
  const normalized = url.trim().toLowerCase();
  if (normalized.startsWith('javascript:')) {
    return '';
  }
  if (normalized.startsWith('data:') && !normalized.startsWith('data:image/')) {
    return '';
  }
  return url;
};

const selectionTouchesRange = (view: any, from: number, to: number) => (
  view.state.selection.ranges.some((range: { from: number; to: number }) => range.from <= to && range.to >= from)
);

const overlapsRange = (ranges: Array<{ from: number; to: number }>, from: number, to: number) => (
  ranges.some((range) => from < range.to && to > range.from)
);

interface CodeMirrorRuntime {
  CodeMirror: React.ComponentType<any>;
  createExtensions: (markdownPath: string, readOnly: boolean) => any[];
}

let codeMirrorRuntimePromise: Promise<CodeMirrorRuntime> | null = null;

const loadCodeMirrorRuntime = () => {
  if (!codeMirrorRuntimePromise) {
    codeMirrorRuntimePromise = Promise.all([
      import('@uiw/react-codemirror'),
      import('@codemirror/commands'),
      import('@codemirror/lang-markdown'),
      import('@codemirror/language'),
      import('@codemirror/state'),
      import('@codemirror/view'),
    ]).then(([
      codeMirrorModule,
      commandsModule,
      markdownModule,
      languageModule,
      stateModule,
      viewModule,
    ]) => {
      const { defaultKeymap, history, historyKeymap, indentWithTab } = commandsModule;
      const { markdown } = markdownModule;
      const { bracketMatching, defaultHighlightStyle, indentOnInput, syntaxHighlighting } = languageModule;
      const { EditorState } = stateModule;
      const { Decoration, EditorView, keymap, ViewPlugin, WidgetType } = viewModule;

      class ImagePreviewWidget extends WidgetType {
        constructor(
          private readonly src: string,
          private readonly alt: string,
          private readonly markdownPath: string,
        ) {
          super();
        }

        toDOM() {
          const figure = document.createElement('figure');
          figure.className = 'markdown-live-image';
          const image = document.createElement('img');
          image.alt = this.alt || '图片';
          figure.appendChild(image);
          resolveImageSrc(this.src, this.markdownPath)
            .then((resolvedSrc) => {
              image.src = safeMarkdownUrlTransform(resolvedSrc);
            })
            .catch((err) => {
              console.error('Error resolving markdown image:', err);
              image.src = safeMarkdownUrlTransform(this.src);
            });
          if (this.alt) {
            const caption = document.createElement('figcaption');
            caption.textContent = this.alt;
            figure.appendChild(caption);
          }
          return figure;
        }

        ignoreEvent() {
          return false;
        }
      }

      const markdownLivePreviewExtension = (markdownPath: string) => ViewPlugin.fromClass(class {
        decorations: any;

        constructor(view: any) {
          this.decorations = this.buildDecorations(view);
        }

        update(update: any) {
          if (update.docChanged || update.selectionSet || update.viewportChanged) {
            this.decorations = this.buildDecorations(update.view);
          }
        }

        buildDecorations(view: any) {
          const decorations = [];
          for (const visibleRange of view.visibleRanges) {
            let position = visibleRange.from;
            while (position <= visibleRange.to) {
              const line = view.state.doc.lineAt(position);
              const text = line.text;
              const headingMatch = text.match(/^(#{1,6})\s+/);
              if (headingMatch) {
                const level = Math.min(headingMatch[1].length, 6);
                decorations.push(Decoration.line({ class: `markdown-live-heading markdown-live-heading-${level}` }).range(line.from));
                const markerFrom = line.from;
                const markerTo = line.from + headingMatch[0].length;
                if (!selectionTouchesRange(view, markerFrom, markerTo)) {
                  decorations.push(Decoration.replace({ class: 'markdown-live-hidden-marker' }).range(markerFrom, markerTo));
                }
              }

              const imageRanges: Array<{ from: number; to: number }> = [];
              const addImageDecoration = (from: number, to: number, src: string, alt: string) => {
                if (!selectionTouchesRange(view, from, to) && !overlapsRange(imageRanges, from, to)) {
                  decorations.push(Decoration.replace({
                    widget: new ImagePreviewWidget(src, alt || src, markdownPath),
                  }).range(from, to));
                  imageRanges.push({ from, to });
                }
              };

              for (const match of text.matchAll(/\[!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)\]\([^)]+\)/g)) {
                const from = line.from + (match.index ?? 0);
                addImageDecoration(from, from + match[0].length, match[2], match[1]);
              }

              for (const match of text.matchAll(/!\[([^\]]*)\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g)) {
                const from = line.from + (match.index ?? 0);
                addImageDecoration(from, from + match[0].length, match[2], match[1]);
              }

              for (const match of text.matchAll(/<img\b[^>]*\bsrc=["']([^"']+)["'][^>]*>/gi)) {
                const from = line.from + (match.index ?? 0);
                const altMatch = match[0].match(/\balt=["']([^"']*)["']/i);
                addImageDecoration(from, from + match[0].length, match[1], altMatch?.[1] || match[1]);
              }

              for (const match of text.matchAll(/\*\*([^*\n]+)\*\*/g)) {
                const start = line.from + (match.index ?? 0);
                const contentFrom = start + 2;
                const contentTo = contentFrom + match[1].length;
                const end = contentTo + 2;
                if (!selectionTouchesRange(view, start, end)) {
                  decorations.push(Decoration.replace({ class: 'markdown-live-hidden-marker' }).range(start, contentFrom));
                  decorations.push(Decoration.mark({ class: 'markdown-live-bold' }).range(contentFrom, contentTo));
                  decorations.push(Decoration.replace({ class: 'markdown-live-hidden-marker' }).range(contentTo, end));
                }
              }

              for (const match of text.matchAll(/(^|[^*])\*([^*\n]+)\*/g)) {
                const matchStart = line.from + (match.index ?? 0);
                const start = matchStart + match[1].length;
                const contentFrom = start + 1;
                const contentTo = contentFrom + match[2].length;
                const end = contentTo + 1;
                if (!selectionTouchesRange(view, start, end)) {
                  decorations.push(Decoration.replace({ class: 'markdown-live-hidden-marker' }).range(start, contentFrom));
                  decorations.push(Decoration.mark({ class: 'markdown-live-italic' }).range(contentFrom, contentTo));
                  decorations.push(Decoration.replace({ class: 'markdown-live-hidden-marker' }).range(contentTo, end));
                }
              }

              if (line.to >= visibleRange.to) {
                break;
              }
              position = line.to + 1;
            }
          }
          return Decoration.set(decorations, true);
        }
      }, {
        decorations: (plugin: any) => plugin.decorations,
      });

      const editorTheme = EditorView.theme({
        '&': {
          minHeight: '100%',
          backgroundColor: 'transparent',
          color: '#4a4642',
          fontSize: '17px',
        },
        '.cm-scroller': {
          minHeight: 'calc(100vh - 210px)',
          paddingBottom: '48px',
          fontFamily: 'Lora, Merriweather, serif',
          lineHeight: '1.8',
        },
        '.cm-content': {
          padding: '18px 0 36px',
        },
        '.cm-line': {
          padding: '0 2px',
        },
        '.cm-gutters': {
          display: 'none',
        },
        '.cm-activeLine': {
          backgroundColor: 'rgba(217, 119, 87, 0.06)',
        },
        '.cm-cursor': {
          borderLeftColor: '#d97757',
        },
        '&.cm-focused': {
          outline: 'none',
        },
      });

      return {
        CodeMirror: codeMirrorModule.default,
        createExtensions: (markdownPath: string, readOnly: boolean) => [
          history(),
          markdown(),
          bracketMatching(),
          indentOnInput(),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
          EditorView.lineWrapping,
          EditorState.readOnly.of(readOnly),
          EditorView.editable.of(!readOnly),
          markdownLivePreviewExtension(markdownPath),
          editorTheme,
        ],
      };
    });
  }
  return codeMirrorRuntimePromise;
};

const useCodeMirrorRuntime = () => {
  const [codeMirrorRuntime, setCodeMirrorRuntime] = useState<CodeMirrorRuntime | null>(null);

  useEffect(() => {
    let mounted = true;
    loadCodeMirrorRuntime()
      .then((runtime) => {
        if (mounted) {
          setCodeMirrorRuntime(runtime);
        }
      })
      .catch((err) => {
        console.error('Error loading CodeMirror runtime:', err);
      });
    return () => {
      mounted = false;
    };
  }, []);

  return codeMirrorRuntime;
};

const useSyncedRef = <T,>(value: T) => {
  const ref = useRef(value);
  useEffect(() => {
    ref.current = value;
  }, [value]);
  return ref;
};

const useMarkdownEditorView = ({ filePath, readOnly = false }: MarkdownEditorProps) => {
  const [fileState, dispatchFileState] = useReducer(editorFileReducer, initialEditorFileState);
  const codeMirrorRuntime = useCodeMirrorRuntime();
  const { content, savedContent, imagePreviewSrc, loading, saveStatus, readError } = fileState;
  const editorViewRef = useRef<any | null>(null);
  const editorShellRef = useRef<HTMLDivElement>(null);
  const latestContentRef = useSyncedRef(content);
  const savedContentRef = useSyncedRef(savedContent);
  const loadingRef = useSyncedRef(loading);
  const readErrorRef = useSyncedRef(readError);
  const lastKnownModifiedAtRef = useRef<number | null>(null);
  const fullSelectionIntentUntilRef = useRef(0);
  const loadRequestIdRef = useRef(0);
  // 补写发生在清理函数里，那时拿不到当帧的 state/props，只能从 ref 取最新的一份。
  const fileStateRef = useSyncedRef(fileState);
  const readOnlyRef = useSyncedRef(readOnly);

  const extensions = useMemo(() => (
    codeMirrorRuntime?.createExtensions(filePath || '', readOnly) ?? []
  ), [codeMirrorRuntime, filePath, readOnly]);
  const CodeMirror = codeMirrorRuntime?.CodeMirror;

  useEffect(() => {
    let mounted = true;
    const requestId = loadRequestIdRef.current + 1;
    loadRequestIdRef.current = requestId;
    const acceptsResponse = () => mounted && loadRequestIdRef.current === requestId;

    if (!filePath) {
      dispatchFileState({ type: 'clear' });
      lastKnownModifiedAtRef.current = null;
      return () => {
        mounted = false;
      };
    }

    if (isImageFile(filePath)) {
      dispatchFileState({ type: 'image-load-start' });
      lastKnownModifiedAtRef.current = null;
      invoke<string>('read_image_data_url', { path: filePath })
        .then((src) => {
          if (acceptsResponse()) {
            dispatchFileState({ type: 'image-load-success', src });
          }
        })
        .catch((err) => {
          console.error('Error reading image:', err);
          if (acceptsResponse()) {
            dispatchFileState({ type: 'image-load-error', error: err });
          }
        });
      return () => {
        mounted = false;
      };
    }

    dispatchFileState({ type: 'text-load-start' });
    Promise.all([
      invoke<string>('read_file', { path: filePath }),
      invoke<number>('file_modified_at', { path: filePath }),
    ])
      .then(([text, modifiedAt]) => {
        if (acceptsResponse()) {
          dispatchFileState({ type: 'text-load-success', content: text, path: filePath });
          lastKnownModifiedAtRef.current = modifiedAt;
        }
      })
      .catch((err) => {
        console.error('Error reading file:', err);
        if (acceptsResponse()) {
          dispatchFileState({ type: 'text-load-error', error: err });
        }
      });

    return () => {
      mounted = false;
    };
  }, [filePath, latestContentRef, loadingRef, readErrorRef, savedContentRef]);

  useEffect(() => {
    if (!filePath || isImageFile(filePath)) {
      return;
    }

    const requestId = loadRequestIdRef.current;
    const pollTimer = window.setInterval(() => {
      if (loadingRef.current || readErrorRef.current || latestContentRef.current !== savedContentRef.current) {
        return;
      }

      invoke<number>('file_modified_at', { path: filePath })
        .then((modifiedAt) => {
          if (lastKnownModifiedAtRef.current === null) {
            lastKnownModifiedAtRef.current = modifiedAt;
            return;
          }

          if (modifiedAt === lastKnownModifiedAtRef.current) {
            return;
          }

          return invoke<string>('read_file', { path: filePath }).then((text) => {
            if (loadRequestIdRef.current !== requestId || latestContentRef.current !== savedContentRef.current) {
              return;
            }

            dispatchFileState({ type: 'external-refresh', content: text });
            lastKnownModifiedAtRef.current = modifiedAt;
          });
        })
        .catch((err) => {
          console.error('Error checking file updates:', err);
        });
    }, 1200);

    return () => {
      window.clearInterval(pollTimer);
    };
  }, [filePath, latestContentRef, loadingRef, readErrorRef, savedContentRef]);

  useEffect(() => {
    if (!shouldPersist(fileState, filePath, readOnly)) {
      return;
    }

    const pathToSave = filePath;
    const contentToSave = content;

    const saveTimer = window.setTimeout(() => {
      invoke<number>('write_file', { path: pathToSave, content: contentToSave })
        .then((modifiedAt) => {
          lastKnownModifiedAtRef.current = modifiedAt;
          dispatchFileState({
            type: 'save-success',
            content: contentToSave,
            isLatest: latestContentRef.current === contentToSave,
          });
        })
        .catch((err) => {
          console.error('Error writing file:', err);
          dispatchFileState({ type: 'save-error' });
        });
    }, 800);

    return () => {
      window.clearTimeout(saveTimer);
    };
  }, [content, fileState, filePath, latestContentRef, loading, readError, readOnly, savedContent]);

  /**
   * 🔴 **离开时把最后那 800 毫秒补上。**
   *
   * 防抖保存的清理函数是 `clearTimeout`，而它的依赖里有 `content`——也就是**每敲一个键**
   * 都会重建计时器。于是「打完最后一句、立刻点开下一章」这个再普通不过的动作，
   * 会在计时器到点前触发清理：**这一轮打的字一个都没落盘，界面上也没有任何提示**。
   * 卸载（离开作品页、关窗）同理。丢的是用户刚写的正文，而且他不会知道。
   *
   * 本 effect 的依赖里**没有 `content`**，所以它的清理只在**换文件或卸载**时跑一次，
   * 正好是那两个丢数据的窗口；内容从 ref 取，永远是最新的一份。
   * 判据与防抖那条共用 `shouldPersist`（含「内容确实来自这个文件」那一条）。
   */
  useEffect(() => {
    const pathAtEntry = filePath;
    return () => {
      const state = fileStateRef.current;
      if (!shouldPersist(state, pathAtEntry, readOnlyRef.current)) {
        return;
      }
      // 这里没有 800ms 可等——组件下一刻就没了，直接落盘。
      invoke('write_file', { path: pathAtEntry, content: state.content }).catch((err) => {
        console.error('Error flushing pending edits:', err);
      });
    };
  }, [filePath, fileStateRef, readOnlyRef]);

  const getSelectedSource = useCallback(() => {
    const view = editorViewRef.current;
    if (!view) {
      return '';
    }
    const ranges: string[] = [];
    for (const range of view.state.selection.ranges) {
      if (!range.empty) {
        ranges.push(view.state.doc.sliceString(range.from, range.to));
      }
    }
    return ranges.join('\n');
  }, []);

  const handleCopy = useCallback((event: ClipboardEvent | React.ClipboardEvent<HTMLDivElement>) => {
    const editorShell = editorShellRef.current;
    const target = event.target as Node | null;
    if (!editorShell || (target && !editorShell.contains(target))) {
      return;
    }

    const selectedSource = getSelectedSource();
    const copiedAfterSelectAll = Date.now() <= fullSelectionIntentUntilRef.current;
    const textToCopy = copiedAfterSelectAll ? latestContentRef.current : selectedSource;
    fullSelectionIntentUntilRef.current = 0;
    if (!textToCopy) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();
    if ('nativeEvent' in event) {
      event.nativeEvent.stopImmediatePropagation();
    } else {
      event.stopImmediatePropagation();
    }
    event.clipboardData?.setData('text/plain', textToCopy);
    event.clipboardData?.setData('text/markdown', textToCopy);
  }, [getSelectedSource, latestContentRef]);
  const handleCopyRef = useRef(handleCopy);

  useEffect(() => {
    handleCopyRef.current = handleCopy;
  }, [handleCopy]);

  const handleEditorKeyDown = useCallback((event: React.KeyboardEvent<HTMLDivElement>) => {
    const target = event.target as Element | null;
    if (!target || !editorShellRef.current?.contains(target)) {
      return;
    }
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'a') {
      fullSelectionIntentUntilRef.current = Date.now() + 5000;
    }
  }, []);

  useEffect(() => {
    const handleDocumentCopy = (event: ClipboardEvent) => {
      handleCopyRef.current(event);
    };
    document.addEventListener('copy', handleDocumentCopy, true);
    return () => {
      document.removeEventListener('copy', handleDocumentCopy, true);
    };
  }, []);

  const insertMarkdown = useCallback((before: string, after = '', placeholder = '') => {
    const view = editorViewRef.current;
    if (!view || readOnly) {
      return;
    }
    const range = view.state.selection.main;
    const selected = view.state.doc.sliceString(range.from, range.to) || placeholder;
    const nextText = `${before}${selected}${after}`;
    view.dispatch({
      changes: { from: range.from, to: range.to, insert: nextText },
      selection: { anchor: range.from + before.length, head: range.from + before.length + selected.length },
    });
    view.focus();
  }, [readOnly]);

  const insertList = useCallback((ordered: boolean) => {
    const view = editorViewRef.current;
    if (!view || readOnly) {
      return;
    }
    const range = view.state.selection.main;
    const selected = view.state.doc.sliceString(range.from, range.to) || '列表项';
    const lines = selected.split('\n');
    const nextText = lines.map((line: string, index: number) => `${ordered ? `${index + 1}.` : '-'} ${line.replace(/^(\s*(?:[-*+]|\d+\.)\s*)/, '')}`).join('\n');
    view.dispatch({
      changes: { from: range.from, to: range.to, insert: nextText },
      selection: { anchor: range.from, head: range.from + nextText.length },
    });
    view.focus();
  }, [readOnly]);

  const insertLink = useCallback(() => {
    const url = window.prompt('请输入链接地址');
    if (!url) {
      return;
    }
    insertMarkdown('[', `](${url})`, '链接文字');
  }, [insertMarkdown]);

  const insertImage = useCallback(() => {
    const src = window.prompt('请输入图片地址，可以是本地路径或互联网地址');
    if (!src) {
      return;
    }
    insertMarkdown('![图片说明](', ')', src);
  }, [insertMarkdown]);

  const handleChange = useCallback((value: string, _viewUpdate: any) => {
    if (!readOnly) {
      dispatchFileState({ type: 'content-changed', content: value });
    }
  }, [readOnly]);

  const isTestMode = import.meta.env.MODE === 'test';

  return (
    <MarkdownEditorView
      CodeMirror={CodeMirror}
      content={content}
      editorShellRef={editorShellRef}
      extensions={extensions}
      filePath={filePath}
      imagePreviewSrc={imagePreviewSrc}
      isImageFile={Boolean(filePath && isImageFile(filePath))}
      isTestMode={isTestMode}
      loading={loading}
      readOnly={readOnly}
      saveStatus={saveStatus}
      onChange={handleChange}
      onContentChange={(nextContent) => dispatchFileState({ type: 'content-changed', content: nextContent })}
      onCopy={handleCopy}
      onEditorKeyDown={handleEditorKeyDown}
      onEditorView={(view) => {
        editorViewRef.current = view;
      }}
      onInsertImage={insertImage}
      onInsertLink={insertLink}
      onInsertList={insertList}
      onInsertMarkdown={insertMarkdown}
    />
  );
};

const MarkdownEditor: React.FC<MarkdownEditorProps> = (props) => useMarkdownEditorView(props);

export default MarkdownEditor;
