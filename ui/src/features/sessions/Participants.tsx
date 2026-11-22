import { useTranslation } from 'react-i18next';

import { Combobox, type ComboboxGroup, type ComboboxOption } from '../../components/ui/Combobox';
import { useCommandErrorHandler } from '../../ipc/errors';
import {
  PERSON_NAME_MAX_LENGTH,
  useAddParticipant,
  usePeople,
  useRemoveParticipant,
  useSessionParticipants,
  type Person,
} from './api';
import { normalizeSearch, personNameKey } from './search';

import './sessions.css';

/** Who took part in the session: the people as chips, plus a field to add one (or a new name). */
export function Participants({ sessionId }: { sessionId: string }) {
  const { t } = useTranslation();
  const handleError = useCommandErrorHandler();
  const people = usePeople();
  const participants = useSessionParticipants(sessionId);
  const add = useAddParticipant(sessionId);
  const remove = useRemoveParticipant(sessionId);

  const label = (person: Person) =>
    person.isSelf ? t('sessions.people.self') : person.displayName;
  const joined = new Set((participants.data ?? []).map((participant) => participant.person.id));
  const candidates = (people.data ?? []).filter((person) => !joined.has(person.id));
  const toOption = (person: Person): ComboboxOption => ({ value: person.id, label: label(person) });

  const self = candidates.find((person) => person.isSelf);
  const groups: ComboboxGroup[] = [
    ...(self ? [{ id: 'self', options: [toOption(self)] }] : []),
    {
      id: 'directory',
      label: t('sessions.people.directory'),
      options: candidates.filter((person) => !person.isSelf).map(toOption),
    },
  ].filter((group) => group.options.length > 0);

  return (
    <section className="note__section participants" aria-labelledby="note-participants">
      <h2 id="note-participants">{t('sessions.participants.title')}</h2>
      {participants.data && participants.data.length > 0 ? (
        <ul className="participants__list" aria-label={t('sessions.participants.listLabel')}>
          {participants.data.map(({ person }) => (
            <li key={person.id} className="participant">
              <span className="participant__name">{label(person)}</span>
              <button
                type="button"
                className="participant__remove"
                aria-label={t('sessions.participants.remove', { name: label(person) })}
                disabled={remove.isPending}
                onClick={() => remove.mutate(person.id, { onError: handleError })}
              >
                <span aria-hidden="true">×</span>
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="note__none">{t('sessions.participants.empty')}</p>
      )}
      <Combobox
        className="participants__add"
        label={t('sessions.participants.addLabel')}
        groups={groups}
        selectedValue={null}
        selectedLabel=""
        placeholder={t('sessions.participants.addPlaceholder')}
        maxLength={PERSON_NAME_MAX_LENGTH}
        normalize={normalizeSearch}
        identityKey={personNameKey}
        createLabel={(name) => t('sessions.people.addNamed', { name })}
        onSelect={(option) => add.mutate({ personId: option.value }, { onError: handleError })}
        onCreate={(name) => add.mutate({ newName: name }, { onError: handleError })}
      />
    </section>
  );
}
