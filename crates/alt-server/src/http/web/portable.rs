//! A portable request of the browser. A download put off to wait for its turn
//! lets go of its store and of the export it read, and is asked again once it
//! has the turn.
use super::execute::Call;
use super::*;
use crate::{git_read::Answer, portable::Step};

impl Call {
    pub(super) fn portable(
        self,
        selected: crate::PortableRoute,
        mut source: alt_os::temp::NamedTempFile,
    ) -> Result<Output> {
        source.as_file_mut().rewind()?;
        let token = self.token.as_deref().ok_or(Error::Unauthorized)?;
        let (factory, browser) = (&self.factory, &self.browser);
        let opened = |source: &mut alt_os::temp::NamedTempFile, sending| {
            let mut store = factory.open_for(None, &self.gone)?;
            store.check_browser_binding(Some(&browser.origin))?;
            let credential = factory.authenticate(&mut store, token)?;
            store.browser_session(&credential, &browser.origin)?;
            let step = crate::portable::execute(
                &mut store,
                &credential,
                &self.options,
                selected.clone(),
                source.as_file_mut(),
                &mut |store| {
                    let current = factory.authenticate(store, token)?;
                    store.browser_session(&current, &browser.origin)?;
                    Ok(current)
                },
                Some(sending),
            )?;
            // a download that waits for its turn holds no connection of the database
            drop(store);
            Ok::<_, Error>(step)
        };
        let mut sending = self.sending.turn_first();
        loop {
            let share = match opened(&mut source, sending)? {
                Step::Done(output) => return Ok(output),
                Step::Download(download) => match download.answered()? {
                    Answer::Done(output) => return Ok(output),
                    Answer::Turn(share) => share,
                },
                Step::Turn(share) => share,
            };
            sending = crate::portable::turned(share)?;
        }
    }
}
