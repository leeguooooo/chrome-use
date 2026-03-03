(() => {
  const REQUEST_TYPE = 'AB_TAB_GROUP_REQUEST';
  const RESPONSE_TYPE = 'AB_TAB_GROUP_RESPONSE';

  window.addEventListener('message', (event) => {
    if (event.source !== window) {
      return;
    }

    const data = event.data;
    if (!data || data.type !== REQUEST_TYPE) {
      return;
    }

    let request;
    try {
      request = {
        type: REQUEST_TYPE,
        nonce: data.nonce,
        session: data.session,
        groupTitle: data.groupTitle,
        pluginId: data.pluginId,
      };
    } catch {
      return;
    }

    try {
      chrome.runtime.sendMessage(request, (response) => {
        const lastError = chrome.runtime.lastError;
        if (lastError) {
          window.postMessage(
            {
              type: RESPONSE_TYPE,
              nonce: request.nonce,
              ok: false,
              error: lastError.message,
            },
            '*'
          );
          return;
        }

        const payload = response && typeof response === 'object' ? response : { ok: false };

        window.postMessage(
          {
            type: RESPONSE_TYPE,
            nonce: request.nonce,
            ok: payload.ok === true,
            extensionId:
              typeof payload.extensionId === 'string' && payload.extensionId.length > 0
                ? payload.extensionId
                : chrome.runtime.id,
            groupId: typeof payload.groupId === 'number' ? payload.groupId : undefined,
            error: typeof payload.error === 'string' ? payload.error : undefined,
          },
          '*'
        );
      });
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      window.postMessage(
        {
          type: RESPONSE_TYPE,
          nonce: request.nonce,
          ok: false,
          error: errorMessage,
        },
        '*'
      );
    }
  });
})();
