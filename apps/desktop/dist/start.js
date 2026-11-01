// The desktop app's start page (CL10): shown while onyx-contrib starts; the app calls showError
// when it cannot start, with a plain-words message.
'use strict';
window.showError = (text) => {
	const p = document.getElementById('msg');
	p.textContent = `The contributor app could not start: ${text}. Close Onyx and open it again; if it keeps failing, reinstall it from the Download page.`;
	p.className = 'bad';
};
