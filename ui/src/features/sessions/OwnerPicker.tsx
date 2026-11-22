import { useTranslation } from 'react-i18next';

import { Combobox, type ComboboxGroup, type ComboboxOption } from '../../components/ui/Combobox';
import { PERSON_NAME_MAX_LENGTH, type Person, type PersonRef } from './api';
import { normalizeSearch, personNameKey } from './search';

const NONE_VALUE = '__none__';

export interface OwnerPickerProps {
  /** The task's text; it tells this picker apart from the others for assistive technology. */
  task: string;
  ownerPersonId: string | null;
  ownerName: string | null;
  ownerIsSelf: boolean;
  /** The whole directory. */
  people: readonly Person[];
  /** Who took part in the session: they are listed before the rest of the directory. */
  participantIds: ReadonlySet<string>;
  disabled?: boolean;
  /**
   * The user's choice: a person of the directory (`person` is its entry), a typed name, or
   * `null` for "no owner". It is not called when the choice is the owner already set.
   */
  onChange: (owner: PersonRef | null, person: Person | null) => void;
}

/** A searchable choice of an action item's owner; a name that matches nobody can be added. */
export function OwnerPicker({
  task,
  ownerPersonId,
  ownerName,
  ownerIsSelf,
  people,
  participantIds,
  disabled,
  onChange,
}: OwnerPickerProps) {
  const { t } = useTranslation();

  const personLabel = (person: Person) =>
    person.isSelf ? t('sessions.people.self') : person.displayName;
  const toOption = (person: Person): ComboboxOption => ({
    value: person.id,
    label: personLabel(person),
  });

  const self = people.find((person) => person.isSelf);
  const others = people.filter((person) => !person.isSelf);
  const groups: ComboboxGroup[] = [
    {
      id: 'none',
      options: [{ value: NONE_VALUE, label: t('sessions.actions.noOwner') }],
    },
    ...(self ? [{ id: 'self', options: [toOption(self)] }] : []),
    {
      id: 'participants',
      label: t('sessions.people.participants'),
      options: others.filter((person) => participantIds.has(person.id)).map(toOption),
    },
    {
      id: 'others',
      label: t('sessions.people.others'),
      options: others.filter((person) => !participantIds.has(person.id)).map(toOption),
    },
  ].filter((group) => group.options.length > 0);

  const hasOwner = ownerPersonId !== null || ownerName !== null;
  const selectedLabel = ownerIsSelf ? t('sessions.people.self') : (ownerName ?? '');

  const select = (option: ComboboxOption) => {
    if (option.value === NONE_VALUE) {
      if (hasOwner) onChange(null, null);
      return;
    }
    if (option.value === ownerPersonId) return;
    const person = people.find((candidate) => candidate.id === option.value);
    if (person) onChange({ personId: person.id }, person);
  };

  return (
    <Combobox
      className="owner-picker"
      label={
        <>
          {t('sessions.actions.ownerLabel')}
          <span className="sr-only">: {task}</span>
        </>
      }
      groups={groups}
      selectedValue={hasOwner ? ownerPersonId : NONE_VALUE}
      selectedLabel={selectedLabel}
      placeholder={t('sessions.actions.noOwner')}
      disabled={disabled}
      maxLength={PERSON_NAME_MAX_LENGTH}
      normalize={normalizeSearch}
      identityKey={personNameKey}
      createLabel={(name) => t('sessions.people.addNamed', { name })}
      onSelect={select}
      onCreate={(name) => onChange({ newName: name }, null)}
    />
  );
}
