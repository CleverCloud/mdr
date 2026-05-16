/*! KDL v2 language definition for highlight.js
 *  KDL Document Language — https://kdl.dev
 *
 *  Highlights:
 *    title.function  — node names
 *    attr            — property keys (key=value)
 *    string          — quoted and raw strings
 *    number          — decimal, hex, octal, binary, float
 *    literal         — #true #false #null #inf #-inf #nan
 *    type            — type annotations (type-name)
 *    comment         — // block and /- slashdash comments
 */
function hljsDefineKdl(hljs) {
  // Bare identifier: any char except whitespace and KDL structural chars.
  // In KDL v2, bare identifiers in value position are not valid, so a bare
  // identifier is either a node name or a property key (followed by =).
  const BARE_ID = /[^\s"#(){}\[\];=,\\][^\s"(){}\[\];=,\\]*/;

  return {
    name: 'KDL',
    aliases: ['kdl'],
    contains: [
      // Line comment: // to end of line
      hljs.COMMENT('//', '$'),

      // Block comment: /* ... */
      // KDL v2 supports nested block comments; this approximates them.
      hljs.C_BLOCK_COMMENT_MODE,

      // Slashdash: comments out the next node, argument, or property
      { scope: 'comment', match: /\/-/ },

      // Type annotation: (type-name) preceding a value or node
      {
        scope: 'type',
        begin: /\(/,
        end: /\)/,
        relevance: 0
      },

      // Quoted string with KDL v2 escape sequences
      {
        scope: 'string',
        begin: /"/,
        end: /"/,
        contains: [{
          scope: 'char.escape',
          // \n \r \t \\ \" \b \f \s  \u{hex}  line-continuation (\ + whitespace + newline)
          match: /\\(?:[nrtbfs\\"\/]|u\{[0-9a-fA-F]{1,6}\}|[ \t]*\r?\n[ \t]*)/
        }]
      },

      // Raw strings: #"..."# or ##"..."## etc.
      // highlight.js cannot enforce matching # counts; this is a close approximation.
      {
        scope: 'string',
        begin: /#+"/,
        end: /"#+/,
        relevance: 0
      },

      // KDL v2 keyword literals
      {
        scope: 'literal',
        match: /#(?:true|false|null|nan|-inf|inf)\b/
      },

      // Numbers — must come before bare identifiers to correctly handle signed literals
      {
        scope: 'number',
        relevance: 0,
        variants: [
          { match: /[+-]?0x[0-9a-fA-F][0-9a-fA-F_]*/ },
          { match: /[+-]?0o[0-7][0-7_]*/ },
          { match: /[+-]?0b[01][01_]*/ },
          { match: /[+-]?\d[\d_]*(?:\.[\d_]+)?(?:[eE][+-]?\d[\d_]*)?\b/ }
        ]
      },

      // Property key: bare identifier immediately before =
      {
        scope: 'attr',
        match: /[^\s"#(){}\[\];=,\\][^\s"(){}\[\];=,\\]*(?=\s*=)/,
        relevance: 0
      },

      // Node name: remaining bare identifiers (in KDL v2, bare identifiers
      // only appear as node names — all other value positions require typed literals)
      {
        scope: 'title.function',
        match: BARE_ID,
        relevance: 0
      }
    ]
  };
}
