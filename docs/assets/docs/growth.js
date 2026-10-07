/* Setup intent only: these events do not claim an install or task success. */
(function () {
  var params = new URLSearchParams(location.search);
  var campaign = params.get('utm_campaign') === 'first-task-20261007'
    ? 'first-task-20261007' : 'organic';
  var source = params.get('utm_source') === 'x' ? 'x' : 'other';
  var medium = params.get('utm_medium') === 'reply' ? 'reply' : 'other';
  function capture(action) {
    var properties = { product: 'chrome-use', action: action,
      campaign: campaign, source: source, medium: medium,
      language: document.documentElement.lang };
    if (typeof window.gtag === 'function') {
      window.gtag('event', 'chrome_use_setup_action', properties);
    }
    if (window.posthog && typeof window.posthog.capture === 'function') {
      window.posthog.capture('chrome_use_setup_action', properties);
    }
  }
  document.querySelectorAll('[data-growth-action]').forEach(function (link) {
    link.addEventListener('click', function () { capture(link.dataset.growthAction); });
  });
  document.querySelectorAll('[data-growth-copy]').forEach(function (button) {
    button.addEventListener('click', function () {
      var task = document.getElementById(button.dataset.growthCopy);
      var status = document.querySelector('[data-growth-status]');
      var en = document.documentElement.lang === 'en';
      if (!task || !navigator.clipboard) {
        if (status) status.textContent = en ? 'Select and copy the task above.' : '请选中上面的任务手动复制。';
        return;
      }
      navigator.clipboard.writeText(task.innerText).then(function () {
        if (status) status.textContent = en ? 'Copied. Paste it into your agent.' : '已复制，粘贴给你的 agent。';
        capture('copy_agent_task');
      }).catch(function () {
        if (status) status.textContent = en ? 'Select and copy the task above.' : '请选中上面的任务手动复制。';
      });
    });
  });
  var demo = document.querySelector('[data-growth-demo]');
  if (demo) demo.addEventListener('play', function () { capture('demo_play'); }, { once: true });
})();
