/**
 * Folds a name for searching: Turkish lower-casing (so "I" and "İ" end up where a Turkish reader
 * expects), then diacritics are dropped ("ş" -> "s", "ğ" -> "g") and the dotless "ı" becomes "i",
 * so a query typed on a keyboard without Turkish letters still finds the name.
 */
export function normalizeSearch(text: string): string {
  return text
    .toLocaleLowerCase('tr')
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .replace(/ı/g, 'i')
    .trim();
}

/** Whether `query` is a part of `text` once both are folded. An empty query matches everything. */
export function matchesSearch(text: string, query: string): boolean {
  const needle = normalizeSearch(query);
  return needle === '' || normalizeSearch(text).includes(needle);
}

/**
 * The key the backend gives a person's name: Turkish lower case, single spaces, accents kept.
 * Two names with the same key are one person, so "Ali" and "Alı" are two.
 */
export function personNameKey(name: string): string {
  return name.toLocaleLowerCase('tr').split(/\s+/).filter(Boolean).join(' ');
}
