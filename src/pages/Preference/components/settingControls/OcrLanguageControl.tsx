import { useMount } from "ahooks";
import { Button, Progress, Radio, Space } from "antd";
import { type FC, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  deleteOcrLanguagePack,
  downloadOcrLanguagePack,
  listOcrLanguagePacks,
  type OcrPackStatus,
} from "@/commands";
import { TAURI_EVENT } from "@/constants/events";
import { OCR_BUILTIN_LANGUAGE_ID } from "@/constants/ocr";
import { useTauriListen } from "@/hooks/useTauriListen";
import type { PreferenceSetting } from "../../types/preferences";
import { formatBytes } from "../../utils/storageUsage";
import type { ControlProps } from "./types";

interface OcrLanguageControlProps extends ControlProps {
  setting: PreferenceSetting;
  value: string;
}

/** 与 Rust `ocr::packs::PackProgress` 一一对应。 */
interface OcrPackProgressPayload {
  id: string;
  received: number;
  total: number;
  error?: string;
}

/**
 * OCR 识别语言选择 + 语言包下载管理。
 * 内置中英(含日文/繁体)恒可选;其余语言需先下载语言包(rec 模型 + 字典)。
 * 下载中状态以 `percents[id]` 为唯一真值;同包并发下载由 Rust 侧 in-flight 守卫兜底。
 */
const OcrLanguageControl: FC<OcrLanguageControlProps> = (props) => {
  const { disabled, onChange, setting, value } = props;
  const { t } = useTranslation("preferences");
  const [packs, setPacks] = useState<OcrPackStatus[]>([]);
  const [percents, setPercents] = useState<Record<string, number>>({});

  const refresh = async () => {
    try {
      setPacks(await listOcrLanguagePacks());
    } catch {
      // 错误 toast 由命令包装层统一处理。
    }
  };

  useMount(() => {
    void refresh();
  });

  const clearProgress = (id: string) => {
    setPercents((prev) => {
      const next = { ...prev };
      delete next[id];
      return next;
    });
    void refresh();
  };

  useTauriListen<OcrPackProgressPayload>(
    TAURI_EVENT.OCR_PACK_PROGRESS,
    (event) => {
      const { id, received, total, error } = event.payload;

      if (error || received >= total) {
        clearProgress(id);
        return;
      }
      setPercents((prev) => {
        return { ...prev, [id]: Math.floor((received / total) * 100) };
      });
    },
  );

  const handleSelect = async (id: string) => {
    await onChange(setting, id);
  };

  const handleDownload = async (id: string) => {
    if (percents[id] !== void 0) {
      return;
    }
    setPercents((prev) => {
      return { ...prev, [id]: 0 };
    });
    try {
      await downloadOcrLanguagePack(id);
    } catch {
      // 错误 toast 已由命令包装层弹出;这里只负责收拾本地状态。
    } finally {
      // 无论成败都清进度:命令在 Rust 早期阶段失败时可能没有终态事件可依赖。
      clearProgress(id);
    }
  };

  const handleDelete = async (id: string) => {
    await deleteOcrLanguagePack(id);
    if (value === id) {
      // 删除当前识别语言的包后,显式切回内置,与 Rust 侧的自动回落保持一致。
      await onChange(setting, OCR_BUILTIN_LANGUAGE_ID);
    }
    await refresh();
  };

  return (
    <div className="flex w-full flex-col gap-2">
      <Radio.Group
        className="flex flex-col gap-2"
        disabled={disabled}
        onChange={(event) => {
          void handleSelect(String(event.target.value));
        }}
        value={value}
      >
        <div className="flex items-center justify-between">
          <Radio value={OCR_BUILTIN_LANGUAGE_ID}>
            {t("snipLanguage.builtin")}
          </Radio>
          <span className="text-ant-tertiary text-xs">
            {t("snipLanguage.builtinNote")}
          </span>
        </div>
        {packs.map((pack) => {
          const percent = percents[pack.id];
          const downloading = percent !== void 0;

          return (
            <div className="flex items-center justify-between" key={pack.id}>
              <Radio disabled={disabled || !pack.downloaded} value={pack.id}>
                {t(`snipLanguage.packs.${pack.id}`)}
                <span className="ml-1 text-ant-tertiary text-xs">
                  {formatBytes(pack.totalBytes)}
                </span>
              </Radio>
              <Space size={8}>
                {downloading ? (
                  <Progress className="w-24" percent={percent} size="small" />
                ) : pack.downloaded ? (
                  <Button
                    disabled={disabled}
                    onClick={() => {
                      void handleDelete(pack.id);
                    }}
                    size="small"
                  >
                    {t("snipLanguage.delete")}
                  </Button>
                ) : (
                  <Button
                    disabled={disabled}
                    onClick={() => {
                      void handleDownload(pack.id);
                    }}
                    size="small"
                    type="primary"
                  >
                    {t("snipLanguage.download")}
                  </Button>
                )}
              </Space>
            </div>
          );
        })}
      </Radio.Group>
    </div>
  );
};

export default OcrLanguageControl;
