const sidebars = {
  docs: [
    'intro',
    {
      type: 'category',
      label: 'User Guide',
      items: [
        {
          type: 'category',
          label: 'Skills',
          collapsed: true,
          items: [
            'reference/skills-catalog',
            'reference/optional-skills-catalog',
          ],
        },
        'after',
      ],
    },
  ],
};
export default sidebars;
