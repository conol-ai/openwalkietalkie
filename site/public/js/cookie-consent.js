(function () {
  'use strict';

  const storageKey = 'owt-cookie-consent-v1';
  const banner = document.getElementById('cookie-consent');
  const settings = document.querySelector('[data-cookie-settings]');
  let choice;
  let requested = false;
  let opener;

  const page = document.documentElement;
  const body = document.body;

  function reserveBannerSpace() {
    if (!banner.hidden) {
      page.style.setProperty('--cookie-banner-height', `${banner.getBoundingClientRect().height}px`);
    }
  }

  function showBanner(show) {
    if (show === !banner.hidden) return;
    const scrollTop = show ? window.scrollY : body.scrollTop;
    banner.hidden = !show;
    if (show) {
      reserveBannerSpace();
      page.classList.add('cookie-banner-open');
      body.scrollTop = scrollTop;
    } else {
      page.classList.remove('cookie-banner-open');
      page.style.removeProperty('--cookie-banner-height');
      window.scrollTo({ top: scrollTop, behavior: 'instant' });
    }
  }

  new ResizeObserver(reserveBannerSpace).observe(banner, { box: 'border-box' });

  function updateConsent(value) {
    window.gtag('consent', 'update', {
      ad_storage: value,
      ad_user_data: value,
      ad_personalization: value,
      analytics_storage: value
    });
  }

  function grantConsent() {
    updateConsent('granted');
    if (requested) return;
    requested = true;
    window.dataLayer.push({ 'gtm.start': Date.now(), event: 'gtm.js' });
    const script = document.createElement('script');
    script.async = true;
    script.src = 'https://www.googletagmanager.com/gtm.js?id=GTM-T7MQBKLT';
    document.head.appendChild(script);
  }

  try {
    choice = localStorage.getItem(storageKey);
  } catch {
    // Storage can be blocked. A choice still applies to this page.
  }

  settings.hidden = false;
  const footer = settings.closest('[data-cookie-settings-footer]');
  if (footer) footer.hidden = false;
  if (choice === 'granted') grantConsent();
  else if (choice !== 'denied') showBanner(true);

  for (const link of document.querySelectorAll('[data-cookie-settings], [data-privacy-open]')) {
    link.addEventListener('click', event => {
      event.preventDefault();
      opener = event.currentTarget;
      showBanner(true);
      banner.querySelector('button').focus();
    });
  }

  banner.addEventListener('keydown', event => {
    if (event.key !== 'Escape' || !opener || !['granted', 'denied'].includes(choice)) return;
    showBanner(false);
    opener.focus();
    opener = undefined;
  });

  for (const button of banner.querySelectorAll('button')) {
    button.addEventListener('click', () => {
      choice = button.value;
      let saved = false;
      try {
        localStorage.setItem(storageKey, choice);
        saved = true;
      } catch {
        // Apply the choice in this document even if it cannot be saved.
      }

      if (choice === 'granted') grantConsent();
      else {
        updateConsent('denied');
        // A fresh denied page also stops custom tags already running in this document.
        if (requested && saved) {
          window.location.reload();
          return;
        }
      }

      showBanner(false);
      (opener || settings).focus();
      opener = undefined;
    });
  }
})();
