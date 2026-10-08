# Store badges

The App Store and Google Play badges the landing page shows for a listed app
(`Day.toml` `[store]`), one directory per locale:
`<locale>/apple-app-store.svg` and `<locale>/google-play-store.svg`.

The files are copied unchanged from the App Fair's `appland` site template
(`public/badges/`, also served at `https://appfair.net/badges/<locale>/…`), which
assembled them from the stores' own localized badge downloads. They ship with this
repository so a site build reads nothing from the network for them.

A directory is named by the locale tag a page uses, which is a Day locale tag
(`zh-CN`, `pt-BR`, `nb`), not the stores' own spelling: the upstream `zh-Hans` and
`zh-Hant` are `zh-CN` and `zh-TW` here, with copies as `zh-SG` and `zh` (Simplified)
and `zh-HK` and `zh-MO` (Traditional), and `no` is copied as `nb`. `badgeLocale()` in
`src/lib/i18n.ts` reads this directory, so adding a locale is adding a folder; a page
locale with no directory of its own falls back to its language, then to `en`.

## Trademarks

Apple, the Apple logo, and App Store are trademarks of Apple Inc. Google Play and
the Google Play logo are trademarks of Google LLC. The artwork is used as the
stores' badge guidelines allow, to link to an app's listing; it is not licensed
under this repository's license.
