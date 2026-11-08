import { useTranslation } from 'react-i18next';

import approving from '../../assets/tan/approving.webp';
import focused from '../../assets/tan/focused.webp';
import happy from '../../assets/tan/happy.webp';
import sittingDesk from '../../assets/tan/sitting-desk.webp';
import thinking from '../../assets/tan/thinking.webp';
import waving from '../../assets/tan/waving.webp';

import './ui.css';

/** Which Tan to show: first run and empty states, waiting, working, a finished note, an error. */
export type TanPose = 'waving' | 'sitting-desk' | 'focused' | 'approving' | 'thinking' | 'happy';

/** Source and intrinsic size (px) of each picture; the ratio keeps the layout from jumping. */
const POSES: Record<TanPose, { src: string; width: number; height: number }> = {
  waving: { src: waving, width: 220, height: 241 },
  'sitting-desk': { src: sittingDesk, width: 220, height: 226 },
  focused: { src: focused, width: 200, height: 261 },
  approving: { src: approving, width: 200, height: 221 },
  thinking: { src: thinking, width: 200, height: 252 },
  happy: { src: happy, width: 200, height: 260 },
};

export interface TanIllustrationProps {
  pose: TanPose;
  /** Rendered width in px; the height follows the picture's ratio. */
  width?: number;
  /** Replaces the default description of the pose. */
  alt?: string;
  /** The picture only decorates a text that already says the same: it is hidden from screen readers. */
  decorative?: boolean;
  className?: string;
}

/** Tan, the purple beaver, in one of its poses. */
export function TanIllustration({
  pose,
  width = 96,
  alt,
  decorative = false,
  className,
}: TanIllustrationProps) {
  const { t } = useTranslation();
  const source = POSES[pose];
  const height = Math.round((width * source.height) / source.width);
  return (
    <img
      className={['tan-illustration', className ?? ''].filter(Boolean).join(' ')}
      src={source.src}
      width={width}
      height={height}
      alt={decorative ? '' : (alt ?? t(`tan.alt.${pose}`))}
      decoding="async"
    />
  );
}
