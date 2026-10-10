import type { Address } from "../types";

/**
 * Format a mailbox for an RFC 5322-style recipient field.
 *
 * Display names are always quoted so punctuation such as commas cannot be
 * mistaken for a separator when the compose field is turned into a list.
 */
export function formatMailbox(address: Address): string {
  if (!address.name) return address.email;

  const displayName = address.name
    .replace(/[\r\n]+/g, " ")
    .replace(/(["\\])/g, "\\$1");
  return `"${displayName}" <${address.email}>`;
}

/**
 * Split a compose recipient field at top-level commas. Commas in quoted
 * display names or angle-bracket addresses belong to the current mailbox.
 */
export function splitAddresses(value: string): string[] {
  const addresses: string[] = [];
  let start = 0;
  let inQuotes = false;
  let escaped = false;
  let angleDepth = 0;

  for (let index = 0; index < value.length; index += 1) {
    const character = value[index];

    if (inQuotes) {
      if (escaped) {
        escaped = false;
      } else if (character === "\\") {
        escaped = true;
      } else if (character === '"') {
        inQuotes = false;
      }
      continue;
    }

    if (character === '"') {
      inQuotes = true;
    } else if (character === "<") {
      angleDepth += 1;
    } else if (character === ">" && angleDepth > 0) {
      angleDepth -= 1;
    } else if (character === "," && angleDepth === 0) {
      const address = value.slice(start, index).trim();
      if (address) addresses.push(address);
      start = index + 1;
    }
  }

  const finalAddress = value.slice(start).trim();
  if (finalAddress) addresses.push(finalAddress);
  return addresses;
}
