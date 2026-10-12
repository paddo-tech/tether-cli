// With reduced motion, each diagram stops on its end state. CSS cannot do this, because SMIL values override CSS.
if (matchMedia('(prefers-reduced-motion: reduce)').matches) {
  for (const svg of document.querySelectorAll<SVGSVGElement>('svg[data-end]')) {
    svg.pauseAnimations();
    svg.setCurrentTime(Number(svg.dataset.end));
  }
}
