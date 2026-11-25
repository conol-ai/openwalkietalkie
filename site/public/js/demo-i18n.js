/* Locale URLs are the preference. No cookies, storage, or browser-language redirects. */
(function () {
  'use strict';
  var catalog = window.OWT_DEMO_STRINGS || {};
  var normalize = function (text) { return text.replace(/\s+/g, ' ').trim(); };
  function translate(text) {
    var key = normalize(text);
    if (Object.prototype.hasOwnProperty.call(catalog, key)) return text.replace(/\S(?:[\s\S]*\S)?/, catalog[key]);
    // Only documented variable slots can match dynamic values; no arbitrary HTML replacement.
    var patterns = window.OWT_DEMO_PATTERNS || [];
    for (var i=0; i<patterns.length; i++) {
      var match = key.match(new RegExp(patterns[i][0]));
      if (match) return patterns[i][1].replace(/\$(\d+)/g, function (_, n) {
        var value = match[Number(n)] || '';
        return value === key ? value : translate(value);
      });
    }
    if (key.includes(" · ")) return key.split(" · ").map(translate).join(" · ");
    return text;
  }
  window.OWTI18n = { t: translate };
})();
