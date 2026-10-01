import { createRoot } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { createInstance } from "i18next";
import { I18nextProvider } from "react-i18next";
import { SWRConfig } from "swr";
import { AuthProvider } from "../../src/hooks/use-auth";
import { StoreCurrencyProvider } from "../../src/hooks/use-store-currency";
import { UsersPage } from "../../src/pages/users";
import { GroupsPage } from "../../src/pages/groups";
import { ProvidersPage } from "../../src/pages/providers";
import { WalletPage } from "../../src/pages/wallet";

const i18n = createInstance();
await i18n.init({ lng: "en", resources: { en: { translation: { common: { retry: "common.retry" } } } } });
const view = new URLSearchParams(location.search).get("view");

createRoot(document.getElementById("root")!).render(
  <I18nextProvider i18n={i18n}>
    <SWRConfig value={{ shouldRetryOnError: false, revalidateOnFocus: false, revalidateOnReconnect: false }}>
      <MemoryRouter>
        <StoreCurrencyProvider>
          <AuthProvider>
            {view === "groups" ? <GroupsPage /> : view === "providers" ? <ProvidersPage /> : view === "wallet" ? <WalletPage /> : <UsersPage />}
          </AuthProvider>
        </StoreCurrencyProvider>
      </MemoryRouter>
    </SWRConfig>
  </I18nextProvider>,
);
