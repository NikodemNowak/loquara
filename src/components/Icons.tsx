import {
  AddRegular,
  ArrowCounterclockwiseRegular,
  BookOpenRegular,
  CheckmarkRegular,
  ChevronDownRegular,
  ColorRegular,
  CopyRegular,
  DeleteRegular,
  DesktopRegular,
  DismissRegular,
  HistoryRegular,
  HomeRegular,
  KeyboardRegular,
  LocalLanguageRegular,
  MicRegular,
  OpenFolderRegular,
  OptionsRegular,
  PauseRegular,
  PlayRegular,
  SearchRegular,
  SettingsRegular,
  StopFilled,
  SubtractRegular,
} from "@fluentui/react-icons";
import type { ComponentType } from "react";

type FluentComponent = ComponentType<{ width?: number | string; height?: number | string; className?: string }>;

export type IconProps = {
  size?: number | string;
  strokeWidth?: number;
  fill?: string;
  className?: string;
};

function wrap(Fluent: FluentComponent) {
  return function Icon({ size = 20, className }: IconProps) {
    return <Fluent width={size} height={size} className={className} aria-hidden="true" />;
  };
}

export const Home = wrap(HomeRegular);
export const Clock3 = wrap(HistoryRegular);
export const Settings = wrap(SettingsRegular);
export const Mic = wrap(MicRegular);
export const Minus = wrap(SubtractRegular);
export const X = wrap(DismissRegular);
export const Search = wrap(SearchRegular);
export const Copy = wrap(CopyRegular);
export const Play = wrap(PlayRegular);
export const Pause = wrap(PauseRegular);
export const Trash2 = wrap(DeleteRegular);
export const RotateCcw = wrap(ArrowCounterclockwiseRegular);
export const Plus = wrap(AddRegular);
export const Check = wrap(CheckmarkRegular);
export const Square = wrap(StopFilled);
export const BookOpen = wrap(BookOpenRegular);
export const Cpu = wrap(DesktopRegular);
export const Languages = wrap(LocalLanguageRegular);
export const FolderOpen = wrap(OpenFolderRegular);
export const Palette = wrap(ColorRegular);
export const SlidersHorizontal = wrap(OptionsRegular);
export const ChevronDown = wrap(ChevronDownRegular);
export const Keyboard = wrap(KeyboardRegular);