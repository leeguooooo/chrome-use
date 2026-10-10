// chrome-use patch (leeguooooo/chrome-use#508): Douyin's APIs send 64-bit ids
// such as item_id as bare JSON numbers, and JSON.parse rounds anything past
// 2^53 (7694857245896576275 -> 7694857245896576000). These helpers quote such
// integers before parsing so they come back as exact strings.
//
// Both functions are also embedded into page scripts with Function#toString
// (BIGINT_JSON_PAGE_SOURCE), so they may reference only each other and
// built-ins.

/**
 * Rewrite JSON text so every integer literal that is not a safe JS integer is
 * quoted. Strings, safe integers, decimals and exponents are left alone.
 */
export function quoteUnsafeIntegers(text) {
    const src = String(text);
    let out = '';
    let i = 0;
    let last = 0;
    while (i < src.length) {
        const c = src[i];
        if (c === '"') {
            i += 1;
            while (i < src.length && src[i] !== '"') i += src[i] === '\\' ? 2 : 1;
            i += 1;
            continue;
        }
        if (c === '-' || (c >= '0' && c <= '9')) {
            const start = i;
            i += 1;
            while (i < src.length && /[0-9.eE+-]/.test(src[i])) i += 1;
            const token = src.slice(start, i);
            if (/^-?\d+$/.test(token) && !Number.isSafeInteger(Number(token))) {
                out += src.slice(last, start) + '"' + token + '"';
                last = i;
            }
            continue;
        }
        i += 1;
    }
    return out + src.slice(last);
}

/** JSON.parse that keeps integers past 2^53 as exact decimal strings. */
export function parseJsonKeepingBigInts(text) {
    return JSON.parse(quoteUnsafeIntegers(text));
}

/** Both helpers as page-script source, for templates that run in the page. */
export const BIGINT_JSON_PAGE_SOURCE = `${quoteUnsafeIntegers.toString()}\n${parseJsonKeepingBigInts.toString()}`;
