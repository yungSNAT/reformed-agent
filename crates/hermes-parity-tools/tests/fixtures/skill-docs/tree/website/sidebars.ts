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
          collapsed: false,
          items: [
            'user-guide/skills/godmode',
            { type: 'doc', id: 'x' },
          ],
        },
        'after',
      ],
    },
  ],
};
export default sidebars;
