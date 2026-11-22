import { describe, expect, it } from 'vitest';

import { matchesSearch, normalizeSearch, personNameKey } from './search';

describe('normalizeSearch', () => {
  it('lower-cases the Turkish way and drops diacritics', () => {
    expect(normalizeSearch('Ayşe')).toBe('ayse');
    expect(normalizeSearch('ÇAĞLAR')).toBe('caglar');
    expect(normalizeSearch('İpek Öz')).toBe('ipek oz');
  });

  it('folds the dotless i so "Irmak" is found by "irmak"', () => {
    expect(normalizeSearch('Irmak')).toBe('irmak');
    expect(normalizeSearch('ISPARTA')).toBe('isparta');
  });

  it('trims the ends', () => {
    expect(normalizeSearch('  Ali ')).toBe('ali');
  });
});

describe('matchesSearch', () => {
  it('finds "Ayşe" by "ayse" and by a part of the name', () => {
    expect(matchesSearch('Ayşe', 'ayse')).toBe(true);
    expect(matchesSearch('Ayşe Yılmaz', 'yilm')).toBe(true);
    expect(matchesSearch('Ayşe', 'AYŞ')).toBe(true);
  });

  it('does not find a different name', () => {
    expect(matchesSearch('Ayşe', 'mehmet')).toBe(false);
  });

  it('matches everything for an empty query', () => {
    expect(matchesSearch('Ayşe', '')).toBe(true);
    expect(matchesSearch('Ayşe', '   ')).toBe(true);
  });
});

describe('personNameKey', () => {
  it('folds case the Turkish way and single-spaces, but keeps accents', () => {
    expect(personNameKey('  AYŞE   Yılmaz ')).toBe('ayşe yılmaz');
    expect(personNameKey('IRMAK')).toBe('ırmak');
  });

  it('keeps names the search would merge apart', () => {
    expect(personNameKey('Ali')).not.toBe(personNameKey('Alı'));
    expect(personNameKey('Ozan')).not.toBe(personNameKey('Özan'));
  });
});
