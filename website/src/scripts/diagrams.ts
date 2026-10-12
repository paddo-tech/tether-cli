// With reduced motion, each diagram stops on its end state. CSS cannot do this, because SMIL values override CSS.
// The setting can change while the page is open, so a change pauses or resumes the diagrams too.
const reduce = matchMedia('(prefers-reduced-motion: reduce)');
const apply = () => {
  for (const svg of document.querySelectorAll<SVGSVGElement>('svg[data-end]')) {
    if (reduce.matches) {
      svg.pauseAnimations();
      svg.setCurrentTime(Number(svg.dataset.end));
    } else {
      svg.unpauseAnimations();
    }
  }
};
apply();
reduce.addEventListener('change', apply);
