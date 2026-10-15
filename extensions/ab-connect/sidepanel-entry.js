import { t } from './i18n.js';

// Configure only Chrome's packaged extension surfaces. No page script is used.
export async function configureAgentPanel(chromeApi) {
  try {
    if (!chromeApi.sidePanel?.setPanelBehavior || !chromeApi.sidePanel?.getPanelBehavior) {
      throw new Error(t('entry_error_side_panel_unsupported'));
    }
    await chromeApi.sidePanel.setPanelBehavior({ openPanelOnActionClick: true });
    const behavior = await chromeApi.sidePanel.getPanelBehavior();
    if (behavior?.openPanelOnActionClick !== true) throw new Error(t('entry_error_side_panel_unconfirmed'));
    await chromeApi.action.setPopup({ popup: '' });
    return { ready: true, error: null };
  } catch (failure) {
    let error = String(failure?.message || failure);
    try { await chromeApi.action.setPopup({ popup: 'popup.html' }); }
    catch (fallback) { error += ` ${t('entry_error_popup_unavailable', fallback?.message || fallback)}`; }
    return { ready: false, error };
  }
}
