//! A portable request of the browser: the download is written once the store
//! is let go of.
use super::execute::Call;
use super::*;
use crate::portable::Step;

impl Call {
    pub(super) fn portable(
        self,
        selected: crate::PortableRoute,
        mut source: alt_os::temp::NamedTempFile,
    ) -> Result<Output> {
        source.as_file_mut().rewind()?;
        let token = self.token.as_deref().ok_or(Error::Unauthorized)?;
        let (factory, browser) = (&self.factory, &self.browser);
        let mut store = factory.open_for(None, &self.gone)?;
        store.check_browser_binding(Some(&browser.origin))?;
        let credential = factory.authenticate(&mut store, token)?;
        store.browser_session(&credential, &browser.origin)?;
        let step = crate::portable::execute(
            &mut store,
            &credential,
            &self.options,
            selected,
            source.as_file_mut(),
            &mut |store| {
                let current = factory.authenticate(store, token)?;
                store.browser_session(&current, &browser.origin)?;
                Ok(current)
            },
            Some(self.sending),
        )?;
        match step {
            Step::Done(output) => Ok(output),
            Step::Download(download) => {
                // a download that waits for its turn holds no connection of the database
                drop(store);
                download.answered()
            }
        }
    }
}
