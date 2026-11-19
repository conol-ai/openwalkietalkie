(function () {
  'use strict';
  var printButton = document.getElementById('printManual');
  if (printButton) printButton.addEventListener('click', function () { window.print(); });
})();
