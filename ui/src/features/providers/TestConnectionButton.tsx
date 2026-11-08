import { useTranslation } from 'react-i18next';

import { Button } from '../../components/ui/Button';
import { CheckIcon, WarningIcon } from '../../components/ui/Icons';
import { describeAiError, useTestProvider, type ProviderProfile } from './api';

import '../../components/ui/ui.css';
import './providers.css';

/**
 * Tests the stored profile and shows the outcome under the button. Parents remount it (via
 * `key`) when the profile's connection settings change, which drops a now stale result.
 */
export function TestConnectionButton({ profile }: { profile: ProviderProfile }) {
  const { t } = useTranslation();
  const test = useTestProvider();

  return (
    <div className="test-connection">
      <Button
        size="sm"
        onClick={() => test.mutate(profile.id)}
        disabled={test.isPending}
        aria-label={t('providers.test.button', { name: profile.displayName })}
      >
        {test.isPending ? t('providers.test.running') : t('providers.test.buttonText')}
      </Button>
      <TestOutcome profile={profile} test={test} />
    </div>
  );
}

function TestOutcome({
  profile,
  test,
}: {
  profile: ProviderProfile;
  test: ReturnType<typeof useTestProvider>;
}) {
  const { t } = useTranslation();

  if (test.isPending) {
    return (
      <p className="test-result" role="status">
        {t('providers.test.running')}
      </p>
    );
  }

  if (test.isError) {
    const { message, detail } = describeAiError(test.error, t);
    return (
      <p className="test-result test-result--error" role="alert">
        <WarningIcon className="test-result__icon" />
        <span>
          <strong>{t('providers.test.failed')}</strong> {message}
        </span>
        {detail ? <span className="test-result__detail">{detail}</span> : null}
      </p>
    );
  }

  if (test.isSuccess) {
    const report = test.data;
    const modelMissing = report.modelFound === false;
    return (
      <p
        className={`test-result ${modelMissing ? 'test-result--error' : 'test-result--ok'}`}
        role={modelMissing ? 'alert' : 'status'}
      >
        {modelMissing ? (
          <WarningIcon className="test-result__icon" />
        ) : (
          <CheckIcon className="test-result__icon" />
        )}
        <span>
          <strong>
            {modelMissing ? t('providers.test.modelMissingTitle') : t('providers.test.success')}
          </strong>{' '}
          {t('providers.test.latency', { ms: report.latencyMs })}
        </span>
        {modelMissing ? (
          <span className="test-result__detail">
            {t('providers.test.modelMissing', { model: profile.model })}
            {report.availableModels.length > 0
              ? ` ${t('providers.test.availableModels', { models: report.availableModels.join(', ') })}`
              : ''}
          </span>
        ) : null}
        {report.modelFound === true ? (
          <span className="test-result__detail">{t('providers.test.modelFound')}</span>
        ) : null}
        {report.replyPreview ? (
          <span className="test-result__detail">
            {t('providers.test.reply', { text: report.replyPreview })}
          </span>
        ) : null}
      </p>
    );
  }

  return null;
}
