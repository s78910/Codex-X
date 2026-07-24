import { useEffect, useState } from "react";
import { Loader2, Pencil, Save } from "lucide-react";

import type { Lang, SkinThemeSummary } from "../types";
import { Button, ModalShell } from "./ui";

export type SkinThemeEditorProps = {
  lang: Lang;
  open: boolean;
  theme: SkinThemeSummary | null;
  busy: boolean;
  onClose: () => void;
  onSave: (id: string, name: string, tagline: string, surfaceOpacity: number) => Promise<boolean>;
};

function getCopy(lang: Lang) {
  return lang === "zh"
    ? {
        title: "编辑皮肤",
        description: "调整皮肤信息和壁纸上方界面的透明度。",
        builtinDescription: "调整壁纸上方界面的透明度。",
        name: "皮肤名称",
        namePlaceholder: "输入皮肤名称",
        tagline: "皮肤简介",
        taglinePlaceholder: "简要描述这套皮肤",
        transparency: "界面透明度",
        cancel: "取消",
        save: "保存",
        saving: "保存中",
        close: "关闭",
      }
    : {
        title: "Edit skin",
        description: "Change skin details and the transparency of surfaces above the wallpaper.",
        builtinDescription: "Adjust the transparency of surfaces above the wallpaper.",
        name: "Skin name",
        namePlaceholder: "Enter a skin name",
        tagline: "Description",
        taglinePlaceholder: "Briefly describe this skin",
        transparency: "Interface transparency",
        cancel: "Cancel",
        save: "Save",
        saving: "Saving",
        close: "Close",
      };
}

export function SkinThemeEditor({
  lang,
  open,
  theme,
  busy,
  onClose,
  onSave,
}: SkinThemeEditorProps) {
  const copy = getCopy(lang);
  const [name, setName] = useState("");
  const [tagline, setTagline] = useState("");
  const [surfaceOpacity, setSurfaceOpacity] = useState(1);
  const editableMetadata = theme?.source !== "builtin";
  const transparency = Math.round((1 - surfaceOpacity) * 100);

  useEffect(() => {
    if (!open || !theme) return;
    setName(theme.name);
    setTagline(theme.tagline);
    setSurfaceOpacity(theme.surfaceOpacity);
  }, [open, theme]);

  const close = () => {
    if (!busy) onClose();
  };
  const save = async () => {
    if (!theme || (editableMetadata && !name.trim()) || busy) return;
    if (await onSave(theme.id, name, tagline, surfaceOpacity)) onClose();
  };

  return (
    <ModalShell
      open={open && Boolean(theme)}
      onClose={close}
      title={<span className="cx-skin-editor-title"><Pencil size={18} aria-hidden="true" />{copy.title}</span>}
      description={editableMetadata ? copy.description : copy.builtinDescription}
      closeLabel={copy.close}
      closeOnBackdrop={!busy}
      closeOnEscape={!busy}
      className="cx-skin-editor-modal"
      bodyClassName="cx-skin-editor-body"
      footer={(
        <>
          <Button variant="secondary" onClick={close} disabled={busy}>{copy.cancel}</Button>
          <Button
            icon={busy ? <Loader2 className="cx-skins-spin" /> : <Save />}
            onClick={() => void save()}
            disabled={busy || (editableMetadata && !name.trim())}
          >
            {busy ? copy.saving : copy.save}
          </Button>
        </>
      )}
    >
      {editableMetadata && (
        <>
          <label className="cx-skin-editor-field">
            <span>{copy.name}</span>
            <input
              value={name}
              onChange={(event) => setName(event.currentTarget.value)}
              placeholder={copy.namePlaceholder}
              maxLength={80}
              disabled={busy}
              data-initial-focus
            />
          </label>
          <label className="cx-skin-editor-field">
            <span>{copy.tagline}</span>
            <textarea
              value={tagline}
              onChange={(event) => setTagline(event.currentTarget.value)}
              placeholder={copy.taglinePlaceholder}
              maxLength={160}
              disabled={busy}
              rows={4}
            />
          </label>
        </>
      )}
      <label className="cx-skin-editor-field">
        <span>{copy.transparency}</span>
        <div className="cx-skin-editor-range">
          <input
            type="range"
            min="0"
            max="65"
            step="1"
            value={transparency}
            onChange={(event) => {
              const value = Number(event.currentTarget.value);
              setSurfaceOpacity(Number((1 - value / 100).toFixed(2)));
            }}
            disabled={busy}
            aria-label={copy.transparency}
            data-initial-focus={!editableMetadata || undefined}
          />
          <output>{transparency}%</output>
        </div>
      </label>
    </ModalShell>
  );
}
