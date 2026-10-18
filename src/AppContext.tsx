import React, { createContext, useContext } from "react";
import type { ThreadInfo, NoteInfo, Compaction, ActionInfo } from "./lib/provider";
import type { Update } from "@tauri-apps/plugin-updater";

export interface ExternalFileAttachment {
  name: string;
  path?: string;
  content: string;
}

export interface AppContextType {
  availableUpdate: Update | null;
  setAvailableUpdate: React.Dispatch<React.SetStateAction<Update | null>>;
  theme: "dark" | "light";
  setTheme: (t: "dark" | "light") => void;
  editMessages: boolean;
  setEditMessages: (v: boolean) => void;
  maxContext: string;
  setMaxContext: (v: string) => void;

  maxToolCalls: string;
  setMaxToolCalls: (v: string) => void;
  readPruneTurns: number | null;
  setReadPruneTurns: (v: number | null) => void;
  permAutoWrites: boolean;
  permAutoCommands: boolean;
  permWhitelist: string[];
  permDraft: string;
  setPermDraft: (v: string) => void;
  updatePerms: (autoWrites: boolean, autoCmds: boolean, list: string[]) => void;
  addPermPattern: () => void;

  profileName: string;
  setProfileName: (v: string) => void;
  profilePreferred: string;
  setProfilePreferred: (v: string) => void;
  profileAbout: string;
  setProfileAbout: (v: string) => void;
  profileShare: boolean;
  setProfileShare: (v: boolean) => void;

  connBaseUrls: Record<string, string>;
  setConnBaseUrls: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  keyStatus: Record<string, boolean>;
  keyDraft: Record<string, string>;
  setKeyDraft: React.Dispatch<React.SetStateAction<Record<string, string>>>;
  saveKey: (id: string) => void;
  deleteKey: (id: string) => void;

  threads: ThreadInfo[];
  setError: (msg: string) => void;

  viewingNote: string | null;
  viewerKind: "note" | "project" | "compaction" | "code";
  noteContent: string;
  noteEditing: boolean;
  noteDraft: string;
  noteRaw: boolean;
  compactionMeta: string | null;
  stagedFiles: string[];
  stageFile: (path: string) => void;
  unstageFile: (path: string) => void;
  toggleStagedFile: (path: string) => void;
  isStaged: (path: string) => boolean;
  stagedExternalFiles: ExternalFileAttachment[];
  stageExternalFile: (file: ExternalFileAttachment) => void;
  unstageExternalFile: (name: string) => void;
  addExternalFiles: (files: FileList | File[]) => Promise<void>;
  onCloseNote: () => void;
  onRenameNote: (name: string) => void;
  onRevealNote: (name: string) => void;
  onRevealFolder?: (path: string) => void;
  onDeleteNote: (name: string) => void;
  onStartNoteEdit: () => void;
  onSaveNote: () => void;
  onExportDocx: () => void;
  setNoteEditing: (v: boolean) => void;
  setNoteRaw: (v: boolean) => void;
  setNoteDraft: (v: string) => void;
  sessionRoot: string | null;
  folderMissing: boolean;
  missingRoots: Set<string>;
  onChangeSessionRoot?: () => void;
  notes: NoteInfo[];
  pages: NoteInfo[];
  projectDocs: string[];
  projectFiles: string[];
  expandedDirs: Set<string>;
  toggleDir: (dirPath: string) => void;
  modifiedFiles: Set<string>;
  showDiff: boolean;
  setShowDiff: (v: boolean) => void;
  fileDiff: string | null;
  loadFileDiff: (path: string) => Promise<void>;
  refreshModifiedFiles: () => Promise<void>;
  added: string[];
  madeInChat: string[];
  compaction: Compaction | null;
  openCompaction: () => void;
  openProjectDoc: (path: string) => void;
  openProjectFile: (path: string) => void;
  refreshProjectFiles: () => Promise<void>;
  openNote: (path: string) => void;
  openPageInBrowser: (path: string) => void;
  openPageSource: (path: string) => void;
  deleteNoteByName: (name: string) => void;
  actions: ActionInfo[];
  refreshActions: () => Promise<void>;
}

export const AppContext = createContext<AppContextType | null>(null);

export function useAppContext(): AppContextType {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useAppContext must be used within AppContext.Provider");
  return ctx;
}
