
import { PurgeCSS } from 'purgecss';

async function checkUnusedCss() {
    console.log('🔍 Checking for unused CSS...');

    const purgeCSSResult = await new PurgeCSS().purge({
        content: ['index.html', 'src/**/*.{tsx,ts,jsx,js,html}'],
        css: ['src/**/*.css'],
        safelist: {
            standard: [
                /^:root/,
                /^body/,
                /^html/,
                /^u-/,     // uPlot classes
            ],
            deep: [],
            greedy: [/^u-/],
        },
        rejected: true,
    });

    let hasUnused = false;

    console.log(`\nFound ${purgeCSSResult.length} CSS files.`);

    for (const fileResult of purgeCSSResult) {
        if (fileResult.rejected && fileResult.rejected.length > 0) {
            hasUnused = true;
            console.log(`\n📄 File: ${fileResult.file}`);
            console.log('⚠️  Unused Selectors:');
            fileResult.rejected.forEach((selector) => {
                console.log(`   - ${selector}`);
            });
        }
    }

    if (hasUnused) {
        console.log('\n❌ Unused CSS found!');
        // In strict CI mode you might want to exit with 1.
        // For now, we exit with 1 to signal "failure" so the user can see it in CI.
        // Use process.exit(0) if you only want a warning.
        process.exit(1);
    } else {
        console.log('\n✅ No unused CSS found.');
    }
}

checkUnusedCss().catch((err) => {
    console.error('Error running PurgeCSS:', err);
    process.exit(1);
});
