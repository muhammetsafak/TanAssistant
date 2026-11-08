import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { TanIllustration } from '../../components/ui/TanIllustration';
import { WarningIcon } from '../../components/ui/Icons';
import { useToast } from '../../components/ui/toast-context';
import { useSettings, useUpdateSettings } from '../settings/useSettings';

import '../../components/ui/ui.css';
import './consent.css';

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

const ICON_PROPS = {
  width: 22,
  height: 22,
  viewBox: '0 0 24 24',
  fill: 'none',
  stroke: 'currentColor',
  strokeWidth: 2,
  strokeLinecap: 'round',
  strokeLinejoin: 'round',
  'aria-hidden': true,
  focusable: false,
  className: 'consent__item-icon',
} as const;

/** Two people: the others in the meeting. */
function PeopleIcon() {
  return (
    <svg {...ICON_PROPS}>
      <circle cx="9" cy="8" r="3" />
      <circle cx="17" cy="9" r="2.5" />
      <path d="M3 20c0-3.3 2.7-6 6-6s6 2.7 6 6M15 14.5c2.8.2 5 2.5 5 5.5" />
    </svg>
  );
}

/**
 * One-time KVKK notice: shown until `consentNoticeAcknowledged` is true. It cannot be dismissed
 * any other way than acknowledging it. If the settings cannot be read the acknowledgement is
 * unknown, so the notice is shown anyway (with the load error) rather than silently skipped.
 */
export function ConsentNotice() {
  const { t } = useTranslation();
  const toast = useToast();
  const { data: settings, isError, refetch } = useSettings();
  const update = useUpdateSettings();

  const open = settings === undefined ? isError : !settings.consentNoticeAcknowledged;
  const loadFailed = settings === undefined && isError;

  function acknowledge() {
    update.mutate({ consentNoticeAcknowledged: true }, { onError: (error) => toast.error(error) });
  }

  if (!open) return null;

  return createPortal(
    <ConsentPanel
      footer={
        <>
          {loadFailed ? <Button onClick={() => void refetch()}>{t('common.retry')}</Button> : null}
          <Button variant="live" data-autofocus onClick={acknowledge} disabled={update.isPending}>
            {t('consent.acknowledge')}
          </Button>
        </>
      }
    >
      {loadFailed ? (
        <p className="field__error" role="alert">
          <WarningIcon className="field__error-icon" />
          {t('errors.settingsLoad')}
        </p>
      ) : null}
    </ConsentPanel>,
    document.body,
  );
}

/**
 * The notice's own modal (the shared `Dialog` is a plain card; this one sits on the horizon scene
 * of the first-run design). Same contract: the rest of the app is inert, focus is trapped, and
 * Escape and a click outside do nothing.
 */
function ConsentPanel({ children, footer }: { children?: ReactNode; footer: ReactNode }) {
  const { t } = useTranslation();
  const panelRef = useRef<HTMLElement>(null);
  const titleId = useId();
  const bodyId = useId();

  useEffect(() => {
    const root = document.getElementById('root');
    const panel = panelRef.current;
    const keepInert = () => {
      if (root && !root.hasAttribute('inert')) root.setAttribute('inert', '');
    };
    keepInert();
    // A shared `Dialog` closing under the notice lifts `inert` from the root; put it back.
    const observer = root ? new MutationObserver(keepInert) : null;
    if (root) observer?.observe(root, { attributes: true, attributeFilter: ['inert'] });
    (panel?.querySelector<HTMLElement>('[data-autofocus]') ?? panel)?.focus();
    return () => {
      observer?.disconnect();
      // Another modal may still be open (the shared `Dialog` keeps its own counter): leave the
      // background inert for it, and let it keep the focus.
      const otherModal = Array.from(
        document.querySelectorAll('[role="dialog"][aria-modal="true"]'),
      ).some((element) => element !== panel);
      if (otherModal) return;
      root?.removeAttribute('inert');
      // The notice's button is gone: hand the focus to the main region, like `Dialog` does.
      document.querySelector<HTMLElement>('main')?.focus();
    };
  }, []);

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (event.key !== 'Tab') return;
    const panel = panelRef.current;
    if (!panel) return;
    const focusable = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE));
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!first || !last) {
      event.preventDefault();
      panel.focus();
      return;
    }
    const active = document.activeElement;
    if (event.shiftKey && (active === first || active === panel)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  return (
    <div className="consent" onKeyDown={handleKeyDown}>
      <div className="consent__horizon consent__horizon--back" aria-hidden="true" />
      <div className="consent__horizon consent__horizon--front" aria-hidden="true" />
      <section
        ref={panelRef}
        className="consent__card"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={bodyId}
        tabIndex={-1}
      >
        <div className="consent__head">
          <TanIllustration pose="waving" width={96} decorative />
          <div className="consent__heading">
            <span className="consent__greeting">{t('consent.greeting')}</span>
            <h2 className="consent__title" id={titleId}>
              {t('consent.title')}
            </h2>
          </div>
        </div>
        <p className="consent__item" id={bodyId}>
          <PeopleIcon />
          <span>{t('consent.body')}</span>
        </p>
        {children}
        <div className="consent__footer">{footer}</div>
      </section>
    </div>
  );
}
