import './scripts/generate-strings.mjs';
export default {
  apiVersion: 1,
  routes: [{ pattern: '/github/oauth', entrypoint: './src/pages/github/oauth.astro', prerender: true }],
};
